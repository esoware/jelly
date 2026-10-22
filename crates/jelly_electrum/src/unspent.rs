use bitcoin::{Amount, OutPoint, Txid};
use serde::{Deserialize, Deserializer};

use crate::history::Confirmation;

/// One entry of `blockchain.scripthash.listunspent`.
///
/// The value is only the server's claim. A caller about to spend must take the
/// value and script from the funding transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unspent {
    pub outpoint: OutPoint,
    pub value: Amount,
    /// `None` for an output that is still in the mempool.
    pub height: Option<u32>,
}

impl<'de> Deserialize<'de> for Unspent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            tx_hash: Txid,
            tx_pos: u32,
            height: i32,
            value: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            outpoint: OutPoint::new(wire.tx_hash, wire.tx_pos),
            value: Amount::from_sat(wire.value),
            height: Confirmation::from_wire(wire.height).height(),
        })
    }
}
