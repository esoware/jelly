use jelly_core::{Amount, CursorBytes, TokenId};
use serde::{Deserialize, Serialize};
use tonlib_core::types::TonAddress;

use crate::client::Position;
use crate::error::TonError;
use crate::history::{Held, Returns};

const SCAN_VERSION: u8 = 2;

/// How far one account's history has been read.
///
/// `latest_lt` is the head of the last completed walk, `head_lt` the head of the walk in
/// progress, `stop_lt` the floor that walk stops at, and `next` the page it fetches next,
/// or `None` once it is complete.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Walk {
    latest_lt: u64,
    head_lt: u64,
    stop_lt: u64,
    next: Option<Position>,
}

impl Walk {
    /// Starts a walk down from `head` unless one is in progress. `floor` reaches back to
    /// before a pending send, so its settlement is matched even if an earlier walk passed
    /// it.
    pub(crate) fn begin(&mut self, head: &Position, floor: Option<u64>) {
        if self.next.is_some() {
            return;
        }
        self.stop_lt = floor.map_or(self.latest_lt, |floor| self.latest_lt.min(floor));
        self.head_lt = head.lt;
        if self.head_lt > self.stop_lt {
            self.next = Some(head.clone());
        }
    }

    /// Takes the page the walk fetches next, or `None` once it is complete.
    pub(crate) const fn take_next(&mut self) -> Option<Position> {
        self.next.take()
    }

    pub(crate) const fn stop_lt(&self) -> u64 {
        self.stop_lt
    }

    /// Moves past a page whose last row read was preceded by `previous`. A page that
    /// reached the floor completes the walk, and any other leaves `previous` to fetch next.
    pub(crate) const fn mark_read(&mut self, previous: Position) {
        if previous.lt <= self.stop_lt {
            self.latest_lt = self.head_lt;
        } else {
            self.next = Some(previous);
        }
    }

    pub(crate) const fn is_at(&self, head_lt: u64) -> bool {
        self.next.is_none() && self.latest_lt >= head_lt
    }
}

/// The owner's wallet under one registry master, its balance as last read, and how far
/// its history has been read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JettonCursor {
    pub(crate) token: TokenId,
    /// The jetton wallet the master names for the owner, written raw.
    wallet: String,
    /// Whether the jetton wallet's own `get_wallet_data` has named the owner and the
    /// master, which it can only once it is deployed.
    is_confirmed: bool,
    balance: Amount,
    /// The jetton wallet's last lt when `balance` was read.
    balance_lt: u64,
    walk: Walk,
}

impl JettonCursor {
    /// A jetton wallet the master has just named, nothing of it read yet.
    pub(crate) fn named(token: TokenId, wallet: &TonAddress) -> Self {
        Self {
            token,
            wallet: wallet.to_hex(),
            is_confirmed: false,
            balance: Amount::ZERO,
            balance_lt: 0,
            walk: Walk::default(),
        }
    }

    pub(crate) fn address(&self) -> Result<TonAddress, TonError> {
        TonAddress::from_hex_str(&self.wallet)
            .map_err(|_| TonError::Internal("a stored jetton wallet does not parse"))
    }

    pub(crate) const fn balance(&self) -> Amount {
        self.balance
    }

    /// Whether the wallet's own data gave the balance when its last lt was `head_lt`, so
    /// reading it again would tell nothing new.
    pub(crate) const fn is_confirmed_at(&self, head_lt: u64) -> bool {
        self.is_confirmed && self.balance_lt == head_lt
    }

    /// Keeps `balance` as the wallet's own data gave it when its last lt was `head_lt`.
    pub(crate) const fn confirm(&mut self, balance: Amount, head_lt: u64) {
        self.is_confirmed = true;
        self.balance = balance;
        self.balance_lt = head_lt;
    }

    /// Keeps that the wallet held nothing when its last lt was `head_lt`, because it was
    /// not deployed.
    pub(crate) const fn empty(&mut self, head_lt: u64) {
        self.balance = Amount::ZERO;
        self.balance_lt = head_lt;
    }
}

/// Which history of one address a walk reads.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Stream {
    Wallet,
    Jetton(usize),
}

/// Everything sync keeps per address, so its field names are a stored format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cursor {
    version: u8,
    wallet: Walk,
    pub(crate) jettons: Vec<JettonCursor>,
    pub(crate) returns: Returns,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            version: SCAN_VERSION,
            wallet: Walk::default(),
            jettons: Vec::new(),
            returns: Returns::default(),
        }
    }
}

impl Cursor {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, TonError> {
        let cursor: Self = serde_json::from_slice(bytes)
            .map_err(|_| TonError::Internal("stored ton history is unreadable"))?;
        if cursor.version != SCAN_VERSION {
            return Err(TonError::Internal("stored ton history has an unsupported format"));
        }
        Ok(cursor)
    }

    pub(crate) fn encode(&self) -> Result<CursorBytes, TonError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| TonError::Internal("the TON history cursor did not serialize"))?;
        Ok(CursorBytes::new(bytes))
    }

    pub(crate) fn held(&self) -> Result<Vec<Held>, TonError> {
        self.jettons
            .iter()
            .map(|jetton| Ok(Held { token: jetton.token, wallet: jetton.address()? }))
            .collect()
    }

    pub(crate) fn walk_mut(&mut self, stream: Stream) -> Result<&mut Walk, TonError> {
        match stream {
            Stream::Wallet => Ok(&mut self.wallet),
            Stream::Jetton(index) => self
                .jettons
                .get_mut(index)
                .map(|jetton| &mut jetton.walk)
                .ok_or(TonError::Internal("a jetton walk went missing")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn a_cursor_round_trips_and_one_of_another_or_no_version_is_refused() {
        let bytes = Cursor::default().encode().unwrap().with_bytes(<[u8]>::to_vec);
        let mut wire: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&Cursor::decode(&bytes).unwrap()).unwrap(), bytes);
        wire["version"] = json!(SCAN_VERSION.saturating_add(1));
        Cursor::decode(&serde_json::to_vec(&wire).unwrap()).unwrap_err();
        wire.as_object_mut().unwrap().remove("version");
        Cursor::decode(&serde_json::to_vec(&wire).unwrap()).unwrap_err();
    }

    #[test]
    fn a_walk_starts_once_and_reaches_back_to_a_pending_floor() {
        let head = Position { lt: 900, hash: [1; 32] };
        let mut walk = Walk { latest_lt: 500, ..Walk::default() };
        walk.begin(&head, Some(300));
        assert_eq!((walk.stop_lt, walk.head_lt, walk.next.clone()), (300, 900, Some(head.clone())));
        walk.begin(&Position { lt: 950, hash: [2; 32] }, None);
        assert_eq!(walk.head_lt, 900, "a walk in progress keeps its head");
        let mut idle = Walk { latest_lt: 900, ..Walk::default() };
        idle.begin(&head, None);
        assert!(idle.is_at(900));
    }
}
