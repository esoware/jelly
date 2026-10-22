//! Fixtures the unit tests share.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_core::{AssetId, ChainId, TokenId};
use solana_pubkey::Pubkey;

use crate::pending::Tracked;
use crate::rpc::{Encoding, RawAccount};
use crate::test_util::{TokenState, token_account_data};
use crate::token::SolanaToken;

/// A recorded mainnet wallet address.
pub(crate) const OWNER: Pubkey =
    Pubkey::from_str_const("9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM");

/// An address outside the wallet.
pub(crate) const PEER: Pubkey =
    Pubkey::from_str_const("5tzFkiKscXHK5ZXCGbXZxdw7gTjjD1mBwuoFbhUvuAi9");

/// [`OWNER`]'s associated USDC address, which is off the curve.
pub(crate) const OFF_CURVE: Pubkey =
    Pubkey::from_str_const("FGETo8T8wMcN2wCjav8VK6eh3dLk63evNDPxzLSJra8B");

/// A token account of `token` that `owner` holds `amount` in, as its program
/// stores it.
pub(crate) fn token_account(
    token: &SolanaToken,
    owner: &Pubkey,
    amount: u64,
    state: TokenState,
) -> RawAccount {
    let data = token_account_data(&token.program(), &token.mint(), owner, amount, state)
        .expect("a packed token account");
    RawAccount { owner: token.program(), data: (BASE64.encode(data), Encoding::Base64) }
}

pub(crate) fn usdc() -> SolanaToken {
    SolanaToken::of(TokenId::UsdcSpl).expect("usdc is carried")
}

/// A native send of a thousand lamports that was signed but not yet handed to an
/// endpoint.
pub(crate) fn tracked(signature: &str, address_id: i64) -> Tracked {
    Tracked {
        signature: signature.to_owned(),
        address_id,
        payer: String::from("payer"),
        asset: AssetId::Native(ChainId::Solana),
        amount: 1_000,
        recipient: String::from("peer"),
        expiry_height: 330,
        raw: String::from("AQID"),
        is_sent: false,
    }
}
