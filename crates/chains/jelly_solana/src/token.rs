//! The compiled SPL token registry, and what a token account at one of its
//! addresses allows a transfer to do.
//!
//! Symbols, decimals and owning programs are trusted configuration, never read
//! from the chain.

use jelly_core::{AssetId, TokenId};
use solana_pubkey::Pubkey;
use solana_system_interface::program::ID as SYSTEM_PROGRAM;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_token_2022_interface::{
    ID as TOKEN_2022_PROGRAM,
    extension::{
        BaseStateWithExtensions as _, ExtensionType, StateWithExtensions,
        account_len::try_calculate_account_len_from_mint_data, memo_transfer::memo_required,
        transfer_fee::TransferFeeConfig, transfer_hook::TransferHook,
    },
    state::{Account, Mint},
};
use spl_token_interface::ID as TOKEN_PROGRAM;

use crate::config::{CHAIN, NATIVE};
use crate::error::SolanaError;
use crate::rpc::RawAccount;

/// Bytes of account data an SPL token account holds, which is what its
/// rent-exempt minimum is quoted against.
const TOKEN_ACCOUNT_BYTES: u64 = 165;

/// Bytes of account data a Token-2022 account of a Paxos mint holds. The mints
/// require transfer-fee and hook state, plus the ATA immutable owner.
const FEE_AND_HOOK_ACCOUNT_BYTES: u64 = 187;

/// Bytes of account data a Token-2022 account of the PUMP mint holds, whose mint
/// has a hook but no transfer fee, so its accounts carry no withheld amount.
const HOOK_ACCOUNT_BYTES: u64 = 175;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SolanaToken {
    id: TokenId,
    mint: Pubkey,
}

impl SolanaToken {
    const fn new(id: TokenId) -> Self {
        Self { id, mint: Pubkey::from_str_const(id.contract()) }
    }

    #[cfg(test)]
    pub(crate) const fn id(self) -> TokenId {
        self.id
    }

    pub(crate) const fn mint(self) -> Pubkey {
        self.mint
    }

    /// The program that owns the mint. It's a seed of every associated token address,
    /// so an unknown mint couldn't be handled without reading it.
    pub(crate) const fn program(self) -> Pubkey {
        if self.is_token_2022() { TOKEN_2022_PROGRAM } else { TOKEN_PROGRAM }
    }

    pub(crate) const fn is_token_2022(self) -> bool {
        self.has_transfer_fee() || matches!(self.id, TokenId::PumpSpl)
    }

    const fn has_transfer_fee(self) -> bool {
        matches!(self.id, TokenId::UsdgSpl | TokenId::PyusdSpl | TokenId::PaxgSpl)
    }

    pub(crate) const fn account_bytes(self) -> u64 {
        if self.has_transfer_fee() {
            FEE_AND_HOOK_ACCOUNT_BYTES
        } else if self.is_token_2022() {
            HOOK_ACCOUNT_BYTES
        } else {
            TOKEN_ACCOUNT_BYTES
        }
    }

    pub(crate) fn validate_mint(self, account: &RawAccount) -> Result<(), SolanaError> {
        let invalid = || SolanaError::TokenConfiguration;
        if account.owner != self.program() {
            return Err(invalid());
        }
        let data = account.bytes().ok_or_else(invalid)?;
        let mint = StateWithExtensions::<Mint>::unpack(&data).map_err(|_| invalid())?;
        if mint.base.decimals != self.decimals() {
            return Err(invalid());
        }
        if !self.is_token_2022() {
            return Ok(());
        }
        let extensions = mint.get_extension_types().map_err(|_| invalid())?;
        if extensions.iter().any(|extension| {
            !matches!(
                extension,
                ExtensionType::MintCloseAuthority
                    | ExtensionType::PermanentDelegate
                    | ExtensionType::TransferFeeConfig
                    | ExtensionType::ConfidentialTransferMint
                    | ExtensionType::ConfidentialTransferFeeConfig
                    | ExtensionType::TransferHook
                    | ExtensionType::MetadataPointer
                    | ExtensionType::TokenMetadata
            )
        }) {
            return Err(invalid());
        }
        if extensions.contains(&ExtensionType::TransferFeeConfig) {
            let fees = mint.get_extension::<TransferFeeConfig>().map_err(|_| invalid())?;
            for fee in [fees.older_transfer_fee, fees.newer_transfer_fee] {
                if u16::from(fee.transfer_fee_basis_points) != 0 && u64::from(fee.maximum_fee) != 0
                {
                    return Err(invalid());
                }
            }
        }
        if extensions.contains(&ExtensionType::TransferHook) {
            let hook = mint.get_extension::<TransferHook>().map_err(|_| invalid())?;
            if Option::<Pubkey>::from(hook.program_id).is_some() {
                return Err(invalid());
            }
        }
        let bytes =
            try_calculate_account_len_from_mint_data(&data, &[ExtensionType::ImmutableOwner])
                .map_err(|_| invalid())?;
        if u64::try_from(bytes).ok() != Some(self.account_bytes()) {
            return Err(invalid());
        }
        Ok(())
    }

    pub(crate) const fn decimals(self) -> u8 {
        self.id.decimals()
    }

    pub(crate) const fn asset(self) -> AssetId {
        AssetId::Token(self.id)
    }

    /// The token with this id, or a refusal for one that lives on another chain.
    pub(crate) fn of(id: TokenId) -> Result<Self, SolanaError> {
        SOLANA_TOKENS
            .into_iter()
            .find(|token| token.id == id)
            .ok_or(SolanaError::ForeignAsset { asset: AssetId::Token(id) })
    }

    /// The associated token account of `owner` for this token.
    pub(crate) fn account_of(self, owner: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, &self.mint, &self.program())
    }

    /// What `owner` holds of this token at one of its associated addresses, or `None`
    /// when that account doesn't exist, holds only lamports, or was reassigned to
    /// another owner.
    pub(crate) fn balance_of(
        self,
        owner: &Pubkey,
        account: Option<&RawAccount>,
    ) -> Result<Option<u64>, SolanaError> {
        Ok(self.held_by(owner, account)?.map(|held| held.amount))
    }

    /// What a transfer can move out of `owner`'s associated account. A frozen account
    /// refuses the transfer.
    pub(crate) fn spendable_by(
        self,
        owner: &Pubkey,
        account: Option<&RawAccount>,
    ) -> Result<u64, SolanaError> {
        match self.held_by(owner, account)? {
            None => Ok(0),
            Some(held) if held.is_frozen => Err(SolanaError::FrozenSource { token: self.id }),
            Some(held) => Ok(held.amount),
        }
    }

    /// Where a transfer of this token to `recipient` credits, or why it mustn't be
    /// sent. `associated` is the recipient's associated address.
    ///
    /// A recipient that is itself a token account of the mint takes the tokens
    /// directly, and any other token account is the wrong token. An off-curve address
    /// that isn't a token account would leave the tokens to a program. Any other
    /// recipient is credited at its associated token account, which must belong to it.
    /// Either way, the account credited must be able to receive.
    pub(crate) fn destination(
        self,
        recipient: Pubkey,
        recipient_account: Option<&RawAccount>,
        associated: Pubkey,
        associated_account: Option<&RawAccount>,
    ) -> Result<Destination, SolanaError> {
        if let Some(account) = recipient_account.filter(|account| is_token_program(account.owner)) {
            let held = TokenAccount::read(account).ok_or(SolanaError::NotARecipient)?;
            if !held.is_of(self) {
                return Err(SolanaError::ForeignMint { token: self.id });
            }
            held.check_receivable(self)?;
            return Ok(Destination { account: recipient, is_open: true });
        }
        if !recipient.is_on_curve() {
            return Err(SolanaError::OffCurveRecipient { token: self.id });
        }
        let Some(held) = self.associated(associated_account)? else {
            return Ok(Destination { account: associated, is_open: false });
        };
        if held.owner != recipient {
            return Err(SolanaError::ForeignTokenAccount { token: self.id });
        }
        held.check_receivable(self)?;
        Ok(Destination { account: associated, is_open: true })
    }

    fn held_by(
        self,
        owner: &Pubkey,
        account: Option<&RawAccount>,
    ) -> Result<Option<TokenAccount>, SolanaError> {
        Ok(self.associated(account)?.filter(|held| held.owner == *owner))
    }

    /// The token account of this mint at one of its associated addresses, or `None`
    /// when there's nothing or only lamports there. The associated-token program
    /// couldn't have made anything else there, so the read is refused rather than taken
    /// as empty.
    fn associated(self, account: Option<&RawAccount>) -> Result<Option<TokenAccount>, SolanaError> {
        let Some(account) = account.filter(|account| account.owner != SYSTEM_PROGRAM) else {
            return Ok(None);
        };
        TokenAccount::read(account)
            .filter(|held| held.is_of(self))
            .map(Some)
            .ok_or(SolanaError::UnreadableTokenAccount { token: self.id })
    }
}

/// The token account a transfer credits, and whether it already exists to take
/// the tokens or the transfer has to create it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Destination {
    pub(crate) account: Pubkey,
    pub(crate) is_open: bool,
}

/// A token account and the rules its own state sets on a transfer.
///
/// Read from base64 with Token-2022's reader, which reads a classic account too,
/// because a classic account is the same layout without the extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TokenAccount {
    /// The token program that owns the account.
    program: Pubkey,
    mint: Pubkey,
    /// The address the balance belongs to. A classic account's owner can be
    /// reassigned, so it need not be the one an associated address was derived
    /// from.
    owner: Pubkey,
    amount: u64,
    /// The mint's freeze authority froze it, so nothing moves in or out.
    is_frozen: bool,
    /// Its `MemoTransfer` extension refuses a transfer in without a memo.
    is_memo_required: bool,
}

impl TokenAccount {
    /// `None` for an account neither token program owns, or whose data is not an
    /// initialized token account, such as a mint.
    fn read(account: &RawAccount) -> Option<Self> {
        if !is_token_program(account.owner) {
            return None;
        }
        let data = account.bytes()?;
        let state = StateWithExtensions::<Account>::unpack(&data).ok()?;
        Some(Self {
            program: account.owner,
            mint: state.base.mint,
            owner: state.base.owner,
            amount: state.base.amount,
            is_frozen: state.base.is_frozen(),
            is_memo_required: memo_required(&state),
        })
    }

    /// Owned by `token`'s program and of its mint.
    fn is_of(&self, token: SolanaToken) -> bool {
        self.program == token.program() && self.mint == token.mint()
    }

    /// A frozen account takes nothing, and one that requires a memo refuses a
    /// transfer without one, which Jelly never attaches.
    const fn check_receivable(&self, token: SolanaToken) -> Result<(), SolanaError> {
        if self.is_frozen {
            return Err(SolanaError::FrozenDestination { token: token.id });
        }
        if self.is_memo_required {
            return Err(SolanaError::MemoRequired { token: token.id });
        }
        Ok(())
    }
}

fn is_token_program(program: Pubkey) -> bool {
    program == TOKEN_PROGRAM || program == TOKEN_2022_PROGRAM
}

/// The Solana tokens Jelly carries, in the order they are reported.
pub(crate) const SOLANA_TOKENS: [SolanaToken; 12] = [
    SolanaToken::new(TokenId::LinkSpl),
    SolanaToken::new(TokenId::WbtcSpl),
    SolanaToken::new(TokenId::EnaSpl),
    SolanaToken::new(TokenId::PumpSpl),
    SolanaToken::new(TokenId::JupSpl),
    SolanaToken::new(TokenId::UsdtSpl),
    SolanaToken::new(TokenId::UsdcSpl),
    SolanaToken::new(TokenId::UsdsSpl),
    SolanaToken::new(TokenId::UsdgSpl),
    SolanaToken::new(TokenId::PyusdSpl),
    SolanaToken::new(TokenId::XautSpl),
    SolanaToken::new(TokenId::PaxgSpl),
];

/// Splits an asset into native lamports or one token, refusing anything that is
/// not Solana's.
pub(crate) fn asset_token(asset: AssetId) -> Result<Option<SolanaToken>, SolanaError> {
    match asset {
        NATIVE => Ok(None),
        AssetId::Token(id) if id.chain() == CHAIN => SolanaToken::of(id).map(Some),
        other @ (AssetId::Native(_) | AssetId::Token(_)) => {
            Err(SolanaError::ForeignAsset { asset: other })
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use jelly_core::ChainId;
    use serde_json::Value;
    use spl_token_2022_interface::extension::{
        BaseStateWithExtensionsMut as _, StateWithExtensionsMut,
    };

    use super::*;
    use crate::address;
    use crate::fixtures::{OFF_CURVE, OWNER, PEER, token_account, usdc};
    use crate::rpc::Encoding;
    use crate::test_util::TokenState;

    const LINK_MINT: &str = "LinkhB3afbBKb2EQQu7s7umdZceV3wcvAUJhQAfQ23L";
    const WBTC_MINT: &str = "5XZw2LKTyrfvfiskJ78AMpackRjPcyCif1WhUsPDuVqQ";
    const ENA_MINT: &str = "72QvBVwpxqmheEPfaCwWSWqEFsUy3rhWt6JhQBMNTwD1";
    const PUMP_MINT: &str = "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn";
    const JUP_MINT: &str = "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN";
    const USDT_MINT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
    const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const USDG_MINT: &str = "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH";
    const USDS_MINT: &str = "USDSwr9ApdHk5bvJKMjzff41FfuX8bSxdKcR81vTwcA";
    const PYUSD_MINT: &str = "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo";
    const XAUT_MINT: &str = "AymATz4TCL9sWNEEV9Kvyz45CHVhDZ6kUgjTJPzLpU9P";
    const PAXG_MINT: &str = "5GgRAEmv8ZxF2PR5hY72Qs5x1bnQ6UK2RbTPoqJ3wSwW";
    const VECTORS: &str = include_str!("../tests/vectors/instructions.json");

    fn token_account_address(owner: &str, token: TokenId) -> Result<String, SolanaError> {
        let owner = address::parse("owner", owner)?;
        Ok(SolanaToken::of(token)?.account_of(&owner).to_string())
    }

    #[test]
    fn the_table_agrees_with_the_asset_registry_it_shadows() {
        let carried: Vec<TokenId> = SOLANA_TOKENS.into_iter().map(SolanaToken::id).collect();
        assert_eq!(
            carried,
            vec![
                TokenId::LinkSpl,
                TokenId::WbtcSpl,
                TokenId::EnaSpl,
                TokenId::PumpSpl,
                TokenId::JupSpl,
                TokenId::UsdtSpl,
                TokenId::UsdcSpl,
                TokenId::UsdsSpl,
                TokenId::UsdgSpl,
                TokenId::PyusdSpl,
                TokenId::XautSpl,
                TokenId::PaxgSpl
            ]
        );
        for token in SOLANA_TOKENS {
            assert_eq!(token.mint().to_string(), token.id().contract());
            assert_eq!(token.decimals(), token.id().decimals());
            assert_eq!(token.id().chain(), ChainId::Solana);
            assert_eq!(SolanaToken::of(token.id()).expect("a solana token"), token);
        }
        let solana: Vec<TokenId> = AssetId::all()
            .filter_map(|asset| match asset {
                AssetId::Token(id) if id.chain() == ChainId::Solana => Some(id),
                AssetId::Native(_) | AssetId::Token(_) => None,
            })
            .collect();
        assert_eq!(solana, carried, "every solana token the registry names is carried");

        // Spelled out here as well, so a typo shared by both tables still fails.
        for (id, mint) in [
            (TokenId::LinkSpl, LINK_MINT),
            (TokenId::WbtcSpl, WBTC_MINT),
            (TokenId::EnaSpl, ENA_MINT),
            (TokenId::PumpSpl, PUMP_MINT),
            (TokenId::JupSpl, JUP_MINT),
            (TokenId::UsdtSpl, USDT_MINT),
            (TokenId::UsdcSpl, USDC_MINT),
            (TokenId::UsdsSpl, USDS_MINT),
            (TokenId::UsdgSpl, USDG_MINT),
            (TokenId::PyusdSpl, PYUSD_MINT),
            (TokenId::XautSpl, XAUT_MINT),
            (TokenId::PaxgSpl, PAXG_MINT),
        ] {
            assert_eq!(
                SolanaToken::of(id).map(|token| token.mint().to_string()).ok(),
                Some(mint.to_owned()),
                "{id:?}"
            );
        }
    }

    fn recorded_mint(name: &str) -> RawAccount {
        let recorded: Value =
            serde_json::from_str(include_str!("../tests/fixtures/mints.json")).unwrap();
        serde_json::from_value(recorded[name].clone()).unwrap()
    }

    #[test]
    fn the_recorded_mints_match_the_registry_and_account_sizes() {
        let pump = SolanaToken::of(TokenId::PumpSpl).unwrap();
        let usdg = SolanaToken::of(TokenId::UsdgSpl).unwrap();
        let usds = SolanaToken::of(TokenId::UsdsSpl).unwrap();
        let pyusd = SolanaToken::of(TokenId::PyusdSpl).unwrap();
        let paxg = SolanaToken::of(TokenId::PaxgSpl).unwrap();
        pump.validate_mint(&recorded_mint("pump")).unwrap();
        assert_eq!(pump.account_bytes(), HOOK_ACCOUNT_BYTES);
        assert_eq!(pump.decimals(), 6);
        assert_ne!(
            pump.account_of(&OWNER),
            get_associated_token_address_with_program_id(&OWNER, &pump.mint(), &TOKEN_PROGRAM)
        );
        paxg.validate_mint(&recorded_mint("paxg")).unwrap();
        assert_eq!(paxg.account_bytes(), FEE_AND_HOOK_ACCOUNT_BYTES);
        assert_eq!(paxg.decimals(), 6);
        assert_ne!(
            paxg.account_of(&OWNER),
            get_associated_token_address_with_program_id(&OWNER, &paxg.mint(), &TOKEN_PROGRAM)
        );
        usdg.validate_mint(&recorded_mint("usdg")).unwrap();
        usds.validate_mint(&recorded_mint("usds")).unwrap();
        pyusd.validate_mint(&recorded_mint("pyusd")).unwrap();
        assert_eq!(usdg.account_bytes(), FEE_AND_HOOK_ACCOUNT_BYTES);
        assert_eq!(usds.account_bytes(), TOKEN_ACCOUNT_BYTES);
        assert_eq!(pyusd.account_bytes(), FEE_AND_HOOK_ACCOUNT_BYTES);
        assert_ne!(
            usdg.account_of(&OWNER),
            get_associated_token_address_with_program_id(&OWNER, &usdg.mint(), &TOKEN_PROGRAM)
        );
    }

    const FEE_AND_HOOK_MINTS: [(TokenId, &str); 2] =
        [(TokenId::UsdgSpl, "usdg"), (TokenId::PaxgSpl, "paxg")];

    #[test]
    fn current_or_scheduled_transfer_fees_stop_a_build() {
        for (id, name) in FEE_AND_HOOK_MINTS {
            let token = SolanaToken::of(id).unwrap();
            for is_newer in [false, true] {
                let mut account = recorded_mint(name);
                let mut data = BASE64.decode(&account.data.0).unwrap();
                let mut mint = StateWithExtensionsMut::<Mint>::unpack(&mut data).unwrap();
                let fees = mint.get_extension_mut::<TransferFeeConfig>().unwrap();
                let fee = if is_newer {
                    &mut fees.newer_transfer_fee
                } else {
                    &mut fees.older_transfer_fee
                };
                fee.transfer_fee_basis_points = 1u16.into();
                fee.maximum_fee = 1u64.into();
                account.data.0 = BASE64.encode(data);
                assert!(
                    matches!(token.validate_mint(&account), Err(SolanaError::TokenConfiguration)),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn an_active_hook_or_unrecognized_extension_stops_a_build() {
        for (id, name) in FEE_AND_HOOK_MINTS.into_iter().chain([(TokenId::PumpSpl, "pump")]) {
            let token = SolanaToken::of(id).unwrap();
            let mut account = recorded_mint(name);
            let mut data = BASE64.decode(&account.data.0).unwrap();
            let mut mint = StateWithExtensionsMut::<Mint>::unpack(&mut data).unwrap();
            mint.get_extension_mut::<TransferHook>().unwrap().program_id = OWNER.into();
            account.data.0 = BASE64.encode(data);
            assert!(
                matches!(token.validate_mint(&account), Err(SolanaError::TokenConfiguration)),
                "{name}"
            );
        }

        let token = SolanaToken::of(TokenId::UsdgSpl).unwrap();
        let mut account = recorded_mint("usdg");
        let mut data = BASE64.decode(&account.data.0).unwrap();
        data.extend(u16::from(ExtensionType::DefaultAccountState).to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.push(2);
        account.data.0 = BASE64.encode(data);
        assert!(matches!(token.validate_mint(&account), Err(SolanaError::TokenConfiguration)));
    }

    #[test]
    fn wrong_program_scale_or_malformed_mint_stops_a_build() {
        let token = SolanaToken::of(TokenId::UsdgSpl).unwrap();
        let mut account = recorded_mint("usdg");
        account.owner = TOKEN_PROGRAM;
        assert!(matches!(token.validate_mint(&account), Err(SolanaError::TokenConfiguration)));
        let mut account = recorded_mint("usdg");
        let mut data = BASE64.decode(&account.data.0).unwrap();
        data[44] = 9;
        account.data.0 = BASE64.encode(data);
        assert!(matches!(token.validate_mint(&account), Err(SolanaError::TokenConfiguration)));
        account.data.0 = "not base64".to_owned();
        assert!(matches!(token.validate_mint(&account), Err(SolanaError::TokenConfiguration)));
    }

    /// Token-2022's reader takes a classic account, which is the same layout with
    /// no extension area.
    #[test]
    fn a_classic_and_a_token_2022_account_read_alike_with_their_rules() {
        let usdg = SolanaToken::of(TokenId::UsdgSpl).unwrap();
        for (token, state) in [
            (usdc(), TokenState::default()),
            (usdc(), TokenState { is_frozen: true, ..TokenState::default() }),
            (usdg, TokenState { is_memo_required: true, ..TokenState::default() }),
        ] {
            let held = TokenAccount::read(&token_account(&token, &PEER, 7, state))
                .expect("a token account");
            assert!(held.is_of(token));
            assert_eq!((held.owner, held.amount), (PEER, 7));
            assert_eq!(held.is_frozen, state.is_frozen);
            assert_eq!(held.is_memo_required, state.is_memo_required);
            assert!(!held.is_of(if token == usdc() { usdg } else { usdc() }));
        }
    }

    #[test]
    fn the_recorded_usdc_account_reads_as_its_owners_balance() {
        let recorded: Value =
            serde_json::from_str(include_str!("../tests/fixtures/token_account_usdc.json"))
                .unwrap();
        let account: RawAccount = serde_json::from_value(recorded["value"].clone()).unwrap();
        let held = TokenAccount::read(&account).expect("a token account");
        assert!(held.is_of(usdc()));
        assert_eq!(held.owner, OWNER);
        assert_eq!(held.amount, 1_095_076_785);
        assert!(!held.is_frozen && !held.is_memo_required);
    }

    #[test]
    fn only_a_token_programs_initialized_account_is_a_token_account() {
        let mut foreign = token_account(&usdc(), &PEER, 7, TokenState::default());
        foreign.owner = SYSTEM_PROGRAM;
        assert_eq!(TokenAccount::read(&foreign), None, "another program's data is not read");

        let mint = recorded_mint("usdg");
        assert_eq!(TokenAccount::read(&mint), None, "a mint is not a token account");

        let mut empty = token_account(&usdc(), &PEER, 0, TokenState::default());
        empty.data.0 = BASE64.encode([0u8; 165]);
        assert_eq!(TokenAccount::read(&empty), None, "an uninitialized account holds nothing");
    }

    #[test]
    fn an_associated_address_is_nothing_lamports_or_this_tokens_account() {
        assert_eq!(usdc().associated(None).unwrap(), None);
        let funded = RawAccount { owner: SYSTEM_PROGRAM, data: (String::new(), Encoding::Base64) };
        assert_eq!(usdc().associated(Some(&funded)).unwrap(), None, "lamports are no account");
        let held = token_account(&usdc(), &PEER, 3, TokenState::default());
        assert_eq!(usdc().associated(Some(&held)).unwrap().map(|held| held.amount), Some(3));
        let usdg = SolanaToken::of(TokenId::UsdgSpl).unwrap();
        assert!(matches!(
            usdg.associated(Some(&held)),
            Err(SolanaError::UnreadableTokenAccount { .. })
        ));
    }

    #[test]
    fn an_owner_holds_what_its_associated_account_holds() {
        let account = token_account(&usdc(), &OWNER, 50_000_000, TokenState::default());
        assert_eq!(usdc().balance_of(&OWNER, Some(&account)).expect("a balance"), Some(50_000_000));
    }

    #[test]
    fn an_owner_with_no_associated_account_holds_nothing_rather_than_failing() {
        assert_eq!(usdc().balance_of(&OWNER, None).expect("a balance"), None);
    }

    /// A classic token account's owner can be reassigned, after which the
    /// balance at the derived address is the new owner's to spend.
    #[test]
    fn an_associated_account_reassigned_away_holds_none_of_the_owners_tokens() {
        let account = token_account(&usdc(), &OWNER, 50_000_000, TokenState::default());
        assert_eq!(usdc().balance_of(&PEER, Some(&account)).expect("a balance"), None);
        assert_eq!(usdc().spendable_by(&PEER, Some(&account)).expect("nothing to spend"), 0);
    }

    /// Anyone can fund an uncreated associated address with lamports, and the
    /// system account that leaves holds no tokens.
    #[test]
    fn an_associated_address_holding_only_lamports_holds_no_tokens() {
        let funded = RawAccount { owner: SYSTEM_PROGRAM, data: (String::new(), Encoding::Base64) };
        assert_eq!(usdc().balance_of(&OWNER, Some(&funded)).expect("a balance"), None);
    }

    #[test]
    fn an_associated_address_holding_another_mints_account_stops_the_read() {
        let usdt = SolanaToken::of(TokenId::UsdtSpl).expect("usdt");
        let account = token_account(&usdt, &OWNER, 1, TokenState::default());
        let error = usdc().balance_of(&OWNER, Some(&account)).expect_err("the wrong mint");
        assert!(matches!(error, SolanaError::UnreadableTokenAccount { .. }), "{error:?}");
    }

    #[test]
    fn a_frozen_account_still_reports_its_balance_but_refuses_to_send() {
        let frozen = token_account(
            &usdc(),
            &OWNER,
            50_000_000,
            TokenState { is_frozen: true, ..TokenState::default() },
        );
        assert_eq!(usdc().balance_of(&OWNER, Some(&frozen)).expect("a balance"), Some(50_000_000));
        assert!(matches!(
            usdc().spendable_by(&OWNER, Some(&frozen)),
            Err(SolanaError::FrozenSource { token: TokenId::UsdcSpl })
        ));
        let open = token_account(&usdc(), &OWNER, 50_000_000, TokenState::default());
        assert_eq!(
            usdc().spendable_by(&OWNER, Some(&open)).expect("a spendable balance"),
            50_000_000
        );
    }

    fn credited(
        recipient: Pubkey,
        recipient_account: Option<&RawAccount>,
        associated_account: Option<&RawAccount>,
    ) -> Result<Destination, SolanaError> {
        usdc().destination(
            recipient,
            recipient_account,
            usdc().account_of(&recipient),
            associated_account,
        )
    }

    #[test]
    fn a_wallet_is_credited_at_its_associated_account_whether_or_not_it_exists() {
        let associated = usdc().account_of(&PEER);
        assert_eq!(
            credited(PEER, None, None).expect("a destination"),
            Destination { account: associated, is_open: false }
        );
        let open = token_account(&usdc(), &PEER, 0, TokenState::default());
        assert_eq!(
            credited(PEER, None, Some(&open)).expect("a destination"),
            Destination { account: associated, is_open: true }
        );
    }

    #[test]
    fn a_token_account_of_the_mint_is_credited_directly() {
        let direct = token_account(&usdc(), &PEER, 0, TokenState::default());
        assert_eq!(
            credited(OFF_CURVE, Some(&direct), None).expect("a destination"),
            Destination { account: OFF_CURVE, is_open: true },
            "no associated address is derived from a token account"
        );
    }

    #[test]
    fn a_frozen_or_memo_requiring_destination_is_refused_wherever_it_is() {
        let frozen = token_account(
            &usdc(),
            &PEER,
            0,
            TokenState { is_frozen: true, ..TokenState::default() },
        );
        assert!(matches!(
            credited(OFF_CURVE, Some(&frozen), None),
            Err(SolanaError::FrozenDestination { .. })
        ));
        assert!(matches!(
            credited(PEER, None, Some(&frozen)),
            Err(SolanaError::FrozenDestination { .. })
        ));

        let usdg = SolanaToken::of(TokenId::UsdgSpl).expect("usdg");
        let memo = token_account(
            &usdg,
            &PEER,
            0,
            TokenState { is_memo_required: true, ..TokenState::default() },
        );
        let refused = usdg
            .destination(OFF_CURVE, Some(&memo), usdg.account_of(&OFF_CURVE), None)
            .expect_err("a memo is required");
        assert!(matches!(refused, SolanaError::MemoRequired { .. }), "{refused:?}");
        assert!(refused.to_string().contains("Jelly can't attach a memo"));
    }

    #[test]
    fn a_token_account_of_another_mint_or_a_program_address_is_refused() {
        let usdt = SolanaToken::of(TokenId::UsdtSpl).expect("usdt");
        let other = token_account(&usdt, &PEER, 0, TokenState::default());
        assert!(matches!(
            credited(OFF_CURVE, Some(&other), None),
            Err(SolanaError::ForeignMint { .. })
        ));

        let vault =
            RawAccount { owner: Pubkey::new_unique(), data: (String::new(), Encoding::Base64) };
        for account in [None, Some(&vault)] {
            let refused = credited(OFF_CURVE, account, None).expect_err("a program address");
            assert!(matches!(refused, SolanaError::OffCurveRecipient { .. }), "{refused:?}");
            assert!(refused.to_string().contains("token account address instead"));
        }

        let mint =
            RawAccount { owner: TOKEN_PROGRAM, data: (BASE64.encode([0u8; 82]), Encoding::Base64) };
        assert!(matches!(credited(PEER, Some(&mint), None), Err(SolanaError::NotARecipient)));
    }

    #[test]
    fn an_associated_account_that_belongs_to_another_address_is_refused() {
        let reassigned = token_account(&usdc(), &OWNER, 0, TokenState::default());
        assert!(matches!(
            credited(PEER, None, Some(&reassigned)),
            Err(SolanaError::ForeignTokenAccount { .. })
        ));
    }

    #[test]
    fn a_token_from_another_chain_is_refused_at_the_edge() {
        assert!(matches!(
            SolanaToken::of(TokenId::UsdtErc20),
            Err(SolanaError::ForeignAsset { .. })
        ));
        assert_eq!(asset_token(AssetId::Native(ChainId::Solana)).expect("native"), None);
        asset_token(AssetId::Native(ChainId::Bitcoin)).unwrap_err();
        asset_token(AssetId::Token(TokenId::UsdtTrc20)).unwrap_err();
        assert_eq!(
            asset_token(AssetId::Token(TokenId::UsdcSpl)).expect("a token").map(SolanaToken::id),
            Some(TokenId::UsdcSpl)
        );
    }

    #[test]
    fn an_associated_address_is_derived_off_the_curve_from_three_seeds() {
        let derived = usdc().account_of(&OWNER);
        assert_eq!(derived.to_string(), "FGETo8T8wMcN2wCjav8VK6eh3dLk63evNDPxzLSJra8B");
        assert!(!derived.is_on_curve());
    }

    #[test]
    fn every_pinned_associated_address_derives_to_the_same_value() {
        let root: Value = serde_json::from_str(VECTORS).unwrap();
        let rows = root
            .get("associatedTokenAddresses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(!rows.is_empty(), "the vector file lists derivations");
        for row in rows {
            let owner = row.get("owner").and_then(Value::as_str).unwrap_or_default();
            let mint = row.get("mint").and_then(Value::as_str).unwrap_or_default();
            let expected = row.get("address").and_then(Value::as_str).unwrap_or_default();
            let token = match mint {
                USDC_MINT => TokenId::UsdcSpl,
                USDT_MINT => TokenId::UsdtSpl,
                other => panic!("the vector names a mint this build does not carry: {other}"),
            };
            assert_eq!(token_account_address(owner, token).unwrap(), expected, "{owner} {mint}");
        }
    }
}
