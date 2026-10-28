//! Fixtures shared by this crate's unit and integration tests. Only
//! `[dev-dependencies]` enable the `test-util` feature.

use jelly_core::EndpointLabel;

/// The EIP-155 example key.
///
/// The EIP publishes its address, `9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f`, and
/// [`OWNER`] is that address in TRON's encoding, so the pair is checked against a
/// source outside this crate.
pub const OWNER_KEY: [u8; 32] = [0x46; 32];
pub const OWNER: &str = "TQLCsShbQNXMTVCjprY64qZmEA4rBarpQp";

/// A recorded mainnet address, the one `account_active.json` was recorded for.
pub const RECIPIENT: &str = "TWd4WrZ9wn84f5x1hZhL4DHvk738ns5jwb";

pub const USDT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";

/// The owner and recipient of recorded mainnet transaction a46244ab. The owner is
/// also the account `native_page1.json` was recorded for.
pub const TRX_OWNER: &str = "TR5NMmWJ9UJvut6hBmrre4Gz28Ez4C1XPK";
pub const TRX_RECIPIENT: &str = "TWJahByCov123LXX6bR2QBsUsh147SP144";
/// [`TRX_OWNER`] as protobuf and the indexer spell it.
pub const TRX_OWNER_HEX: &str = "41a5b43182dad15ff0b707d829ffa67b1af6719483";
/// [`TRX_RECIPIENT`] as protobuf and the indexer spell it.
pub const TRX_RECIPIENT_HEX: &str = "41df0c86c531205eb7a4e27d9589ba7188dfb32f08";

/// The endpoint every scripted node answers as.
pub fn endpoint() -> EndpointLabel {
    EndpointLabel::new("https", "node.invalid", None)
}

/// What `getaccountresource` answers for an account with the given pools.
pub fn account_resource(free_used: u64, staked_limit: u64, energy_limit: u64) -> String {
    format!(
        r#"{{"freeNetLimit":600,"freeNetUsed":{free_used},"NetLimit":{staked_limit},
            "EnergyLimit":{energy_limit},"TotalNetLimit":43200000000,
            "TotalEnergyLimit":180000000000}}"#
    )
}
