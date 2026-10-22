//! Fixtures this crate's unit tests and integration tests share. Only
//! `[dev-dependencies]` enable the `test-util` feature.

use jelly_core::EndpointLabel;
use solana_pubkey::Pubkey;
use spl_token_2022_interface::{
    ID as TOKEN_2022_PROGRAM,
    extension::{
        BaseStateWithExtensionsMut as _, ExtensionType, StateWithExtensionsMut,
        immutable_owner::ImmutableOwner, memo_transfer::MemoTransfer,
    },
    state::{Account, AccountState},
};

/// The endpoint every scripted node answers as.
pub fn endpoint() -> EndpointLabel {
    EndpointLabel::new("https", "node.invalid", None)
}

/// The state a scripted token account is packed with.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenState {
    pub is_frozen: bool,
    pub is_memo_required: bool,
}

/// The data of a token account of `mint` that `owner` holds `amount` in, packed
/// with the SDK the way `program` stores it, or `None` when the SDK refuses.
///
/// A Token-2022 account carries the immutable owner every associated account
/// has, and the memo rule when `state` asks for it.
pub fn token_account_data(
    program: &Pubkey,
    mint: &Pubkey,
    owner: &Pubkey,
    amount: u64,
    state: TokenState,
) -> Option<Vec<u8>> {
    let is_token_2022 = *program == TOKEN_2022_PROGRAM;
    let mut extensions = Vec::new();
    if is_token_2022 {
        extensions.push(ExtensionType::ImmutableOwner);
        if state.is_memo_required {
            extensions.push(ExtensionType::MemoTransfer);
        }
    }
    let length = ExtensionType::try_calculate_account_len::<Account>(&extensions).ok()?;
    let mut data = vec![0u8; length];
    let mut packed = StateWithExtensionsMut::<Account>::unpack_uninitialized(&mut data).ok()?;
    if is_token_2022 {
        packed.init_extension::<ImmutableOwner>(true).ok()?;
        if state.is_memo_required {
            packed.init_extension::<MemoTransfer>(true).ok()?.require_incoming_transfer_memos =
                true.into();
        }
    }
    packed.base = Account {
        mint: *mint,
        owner: *owner,
        amount,
        state: if state.is_frozen { AccountState::Frozen } else { AccountState::Initialized },
        ..Account::default()
    };
    packed.pack_base();
    if is_token_2022 {
        packed.init_account_type().ok()?;
    }
    Some(data)
}
