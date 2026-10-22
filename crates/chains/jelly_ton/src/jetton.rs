//! The TEP-74 jettons this wallet carries, and the message bodies their history is read
//! from.
//!
//! The bodies are laid out as
//! <https://github.com/ton-blockchain/TEPs/blob/master/text/0074-jettons-standard.md>
//! lays them out. Symbols and decimals come from the compiled [`TokenId`] table, never
//! from the chain.

use jelly_core::{AssetId, TokenId};
use tonlib_core::cell::{Cell, CellParser};
use tonlib_core::types::TonAddress;

use crate::config::CHAIN;
use crate::error::TonError;

pub(crate) const TRANSFER: u32 = 0x0f8a_7ea5;
pub(crate) const INTERNAL_TRANSFER: u32 = 0x178d_4519;
const TRANSFER_NOTIFICATION: u32 = 0x7362_d09c;
const EXCESSES: u32 = 0xd532_76db;
/// The prefix of every bounced body, which carries the first 256 bits of the body that
/// bounced after it.
const BOUNCED: u32 = 0xffff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Jetton {
    pub(crate) token: TokenId,
    /// The Gram a transfer attaches for the jetton wallets' gas, as `MyTonWallet`'s
    /// `getToncoinAmountForTransfer` attaches it. The jetton wallets return what they
    /// do not spend as an excess.
    pub(crate) attached_nanogram: u64,
}

pub(crate) const JETTONS: [Jetton; 2] = [
    Jetton { token: TokenId::UsdtJetton, attached_nanogram: 18_000_000 },
    Jetton { token: TokenId::XautJetton, attached_nanogram: 50_000_000 },
];

pub(crate) fn jetton(token: TokenId) -> Option<Jetton> {
    JETTONS.into_iter().find(|jetton| jetton.token == token)
}

/// The jetton an asset names, or `None` for native Gram.
pub(crate) fn asset_jetton(asset: AssetId) -> Result<Option<Jetton>, TonError> {
    match asset {
        AssetId::Native(CHAIN) => Ok(None),
        AssetId::Token(token) => {
            jetton(token).map(Some).ok_or(TonError::Invalid("Choose an asset on TON."))
        }
        AssetId::Native(_) => Err(TonError::Invalid("Choose an asset on TON.")),
    }
}

pub(crate) fn master(token: TokenId) -> Result<TonAddress, TonError> {
    TonAddress::from_base64_url(token.contract())
        .map_err(|_| TonError::Internal("a jetton master address does not parse"))
}

/// What a message body says, as far as jetton history reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Body {
    Transfer { query_id: u64, amount: u128, destination: TonAddress },
    InternalTransfer { amount: u128, from: TonAddress },
    Notification,
    Excesses { query_id: u64 },
    Bounced { op: u32, query_id: u64 },
    Other,
}

/// Reads the head of `body` and nothing past the fields history needs, so a body that
/// carries more than the standard lays out still reads.
pub(crate) fn read_body(body: &Cell) -> Body {
    let mut parser = body.parser();
    read(&mut parser).unwrap_or(Body::Other)
}

fn read(parser: &mut CellParser<'_>) -> Option<Body> {
    let op = parser.load_u32(32).ok()?;
    match op {
        TRANSFER => {
            let query_id = parser.load_u64(64).ok()?;
            let amount = u128::try_from(&parser.load_coins().ok()?).ok()?;
            let destination = parser.load_address().ok()?;
            Some(Body::Transfer { query_id, amount, destination })
        }
        INTERNAL_TRANSFER => {
            parser.load_u64(64).ok()?;
            let amount = u128::try_from(&parser.load_coins().ok()?).ok()?;
            let from = parser.load_address().ok()?;
            Some(Body::InternalTransfer { amount, from })
        }
        TRANSFER_NOTIFICATION => Some(Body::Notification),
        EXCESSES => Some(Body::Excesses { query_id: parser.load_u64(64).ok()? }),
        BOUNCED => {
            let op = parser.load_u32(32).ok()?;
            let query_id = parser.load_u64(64).ok()?;
            Some(Body::Bounced { op, query_id })
        }
        _ => Some(Body::Other),
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::ChainId;

    use super::*;

    #[test]
    fn the_jetton_table_is_total_over_the_tokens_this_chain_serves() {
        let served: Vec<TokenId> =
            TokenId::ALL.into_iter().filter(|token| token.chain() == ChainId::Ton).collect();
        assert_eq!(served, JETTONS.map(|jetton| jetton.token));
        for jetton in JETTONS {
            master(jetton.token).unwrap();
            assert_eq!(asset_jetton(AssetId::Token(jetton.token)).unwrap(), Some(jetton));
        }
        assert_eq!(asset_jetton(AssetId::Native(ChainId::Ton)).unwrap(), None);
        asset_jetton(AssetId::Token(TokenId::UsdtTrc20)).unwrap_err();
        asset_jetton(AssetId::Native(ChainId::Tron)).unwrap_err();
    }

    #[test]
    fn each_jetton_attaches_what_mytonwallet_attaches() {
        // MyTonWallet's TINY_TOKEN_TRANSFER_AMOUNT for USDT and TOKEN_TRANSFER_AMOUNT for
        // every other jetton, in src/api/chains/ton/constants.ts.
        assert_eq!(jetton(TokenId::UsdtJetton).unwrap().attached_nanogram, 18_000_000);
        assert_eq!(jetton(TokenId::XautJetton).unwrap().attached_nanogram, 50_000_000);
    }
}
