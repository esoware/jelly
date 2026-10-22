use jelly_core::AssetId;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_message::{VersionedMessage, v0};
use solana_pubkey::Pubkey;
use solana_system_interface::instruction as system_instruction;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use spl_token_2022_interface::extension::transfer_fee::instruction::transfer_checked_with_fee;
use spl_token_interface::instruction::transfer_checked;

use crate::config::NATIVE;
use crate::error::SolanaError;
use crate::fee::ComputeUnits;
use crate::token::SolanaToken;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Leg {
    Native,
    Token { token: SolanaToken, source: Pubkey, destination: Pubkey },
}

impl Leg {
    pub(crate) const fn asset(self) -> AssetId {
        match self {
            Self::Native => NATIVE,
            Self::Token { token, .. } => token.asset(),
        }
    }
}

/// The accounts one transfer touches.
///
/// Both associated addresses are derived locally, because the token program is
/// one of the seeds and is compiled in for every carried mint.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Route {
    pub(crate) payer: Pubkey,
    pub(crate) recipient: Pubkey,
    pub(crate) leg: Leg,
    /// True when the account that decides this transfer's shape exists and is
    /// usable. That is the recipient's own account for a native transfer, and a
    /// token account owned by the mint's program for a token one.
    ///
    /// False until the build reads say otherwise, so a transfer carries a
    /// creation it may not need rather than lacking one it does.
    pub(crate) has_destination_account: bool,
}

impl Route {
    pub(crate) const fn native(payer: Pubkey, recipient: Pubkey) -> Self {
        Self { payer, recipient, leg: Leg::Native, has_destination_account: false }
    }

    pub(crate) fn token(payer: Pubkey, recipient: Pubkey, token: SolanaToken) -> Self {
        Self {
            payer,
            recipient,
            leg: Leg::Token {
                token,
                source: token.account_of(&payer),
                destination: token.account_of(&recipient),
            },
            has_destination_account: false,
        }
    }

    /// True when this transfer has to create the recipient's token account and
    /// deposit its rent.
    pub(crate) const fn is_account_creation(&self) -> bool {
        matches!(self.leg, Leg::Token { .. }) && !self.has_destination_account
    }

    pub(crate) const fn shape(&self) -> Shape {
        let kind = match self.leg {
            Leg::Native => TransferKind::Native,
            Leg::Token { token, .. } if token.is_token_2022() => TransferKind::Token2022,
            Leg::Token { .. } => TransferKind::Token,
        };
        Shape { kind, is_account_creation: self.is_account_creation() }
    }

    /// The accounts a prioritization-fee sample is asked about, because the whole
    /// cluster's floor would let one unrelated hot account price a plain transfer.
    pub(crate) fn writable_accounts(&self) -> Vec<String> {
        match self.leg {
            Leg::Native => vec![self.payer.to_string(), self.recipient.to_string()],
            Leg::Token { source, destination, .. } => {
                vec![self.payer.to_string(), source.to_string(), destination.to_string()]
            }
        }
    }

    /// The account creation comes before the transfer that credits it. It is the
    /// idempotent variant, so a recipient account that appears between the
    /// existence check and the transaction landing makes it a no-op instead of
    /// failing the transfer. The transfer is the checked variant, which carries
    /// the mint and decimals, so the token program refuses a transfer priced at
    /// the wrong scale instead of moving the wrong amount.
    ///
    /// Both compute-budget instructions are always present, whatever the price,
    /// so the simulation that measures the limit measures the transaction that
    /// will be sent.
    fn instructions(
        &self,
        amount: u64,
        units: ComputeUnits,
        micro_lamports_per_unit: u64,
    ) -> Result<Vec<Instruction>, SolanaError> {
        let mut instructions = Vec::with_capacity(4);
        instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(units.limit()));
        instructions
            .push(ComputeBudgetInstruction::set_compute_unit_price(micro_lamports_per_unit));
        match self.leg {
            Leg::Native => {
                instructions.push(system_instruction::transfer(
                    &self.payer,
                    &self.recipient,
                    amount,
                ));
            }
            Leg::Token { token, source, destination } => {
                if self.is_account_creation() {
                    instructions.push(create_associated_token_account_idempotent(
                        &self.payer,
                        &self.recipient,
                        &token.mint(),
                        &token.program(),
                    ));
                }
                let transfer = if token.is_token_2022() {
                    // The signed zero fee prevents an issuer fee change from reducing the payment.
                    transfer_checked_with_fee(
                        &token.program(),
                        &source,
                        &token.mint(),
                        &destination,
                        &self.payer,
                        &[],
                        amount,
                        token.decimals(),
                        0,
                    )
                } else {
                    transfer_checked(
                        &token.program(),
                        &source,
                        &token.mint(),
                        &destination,
                        &self.payer,
                        &[],
                        amount,
                        token.decimals(),
                    )
                }
                .map_err(|_| SolanaError::Encode { what: "token transfer" })?;
                instructions.push(transfer);
            }
        }
        Ok(instructions)
    }

    /// A v0 message even with no address lookup tables, because a legacy message
    /// buys nothing here.
    pub(crate) fn compile(
        &self,
        amount: u64,
        units: ComputeUnits,
        micro_lamports_per_unit: u64,
        blockhash: Hash,
    ) -> Result<VersionedMessage, SolanaError> {
        let instructions = self.instructions(amount, units, micro_lamports_per_unit)?;
        let message = v0::Message::try_compile(&self.payer, &instructions, &[], blockhash)
            .map_err(SolanaError::Compile)?;
        Ok(VersionedMessage::V0(message))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransferKind {
    Native,
    Token,
    Token2022,
}

/// What makes two transfers consume the same compute units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) kind: TransferKind,
    pub(crate) is_account_creation: bool,
}

#[cfg(test)]
mod tests {
    use jelly_core::TokenId;
    use solana_compute_budget_interface::ID as COMPUTE_BUDGET_PROGRAM;
    use solana_system_interface::program::ID as SYSTEM_PROGRAM;
    use spl_token_2022_interface::extension::transfer_fee::instruction::TransferFeeInstruction;
    use spl_token_2022_interface::instruction::TokenInstruction;

    use super::*;
    use crate::fee::UnitSource;
    use crate::fixtures::{OWNER, PEER, usdc};
    use crate::program;

    fn units(limit: u32) -> ComputeUnits {
        ComputeUnits::new(limit, UnitSource::Simulated).expect("a limit")
    }

    #[test]
    fn a_native_route_writes_to_the_two_accounts_it_names() {
        let route = Route::native(OWNER, PEER);
        assert!(!route.is_account_creation(), "a native transfer never creates a token account");
        assert_eq!(route.writable_accounts(), vec![OWNER.to_string(), PEER.to_string()]);
        assert_eq!(route.shape(), Shape { kind: TransferKind::Native, is_account_creation: false });
        assert_eq!(route.instructions(1, units(500), 0).expect("instructions").len(), 3);
    }

    /// A route assumes the destination is missing until told otherwise, because an
    /// unneeded creation is a no-op and a missing one fails the transfer.
    #[test]
    fn a_token_route_writes_to_both_token_accounts_and_never_to_the_recipient() {
        let mut route = Route::token(OWNER, PEER, usdc());
        let writable = route.writable_accounts();
        assert_eq!(writable.len(), 3);
        assert!(!writable.contains(&PEER.to_string()), "{writable:?}");
        assert_eq!(route.shape(), Shape { kind: TransferKind::Token, is_account_creation: true });
        let instructions = route.instructions(1, units(500), 0).expect("instructions");
        assert_eq!(instructions.len(), 4, "creation is included");

        route.has_destination_account = true;
        assert_eq!(route.instructions(1, units(500), 0).expect("instructions").len(), 3);
        assert_eq!(route.shape(), Shape { kind: TransferKind::Token, is_account_creation: false });
    }

    #[test]
    fn usdg_uses_its_own_program_and_a_signed_zero_transfer_fee() {
        let token = SolanaToken::of(TokenId::UsdgSpl).unwrap();
        let mut route = Route::token(OWNER, PEER, token);
        assert_eq!(
            route.shape(),
            Shape { kind: TransferKind::Token2022, is_account_creation: true }
        );
        assert_ne!(route.shape(), Route::token(OWNER, PEER, usdc()).shape());
        for has_destination in [false, true] {
            route.has_destination_account = has_destination;
            let instructions = route.instructions(2_500_000, units(80_000), 0).unwrap();
            let transfer = instructions.last().unwrap();
            assert_eq!(transfer.program_id, token.program());
            assert!(matches!(
                TokenInstruction::unpack(&transfer.data),
                Ok(TokenInstruction::TransferFeeExtension)
            ));
            assert_eq!(
                TransferFeeInstruction::unpack(&transfer.data[1..]).unwrap(),
                TransferFeeInstruction::TransferCheckedWithFee {
                    amount: 2_500_000,
                    decimals: 6,
                    fee: 0
                }
            );
            assert_eq!(transfer.accounts[0].pubkey, token.account_of(&OWNER));
            assert_eq!(transfer.accounts[2].pubkey, token.account_of(&PEER));
            assert_eq!(instructions.len(), if has_destination { 3 } else { 4 });
            let message = route.compile(2_500_000, units(80_000), 0, Hash::default()).unwrap();
            let VersionedMessage::V0(message) = message else { panic!("a v0 message") };
            let decoded = program::decode_message(&message);
            let last = decoded.last().unwrap();
            assert_eq!(last.program, "Token-2022");
            assert!(last.summary.contains("checked transfer of 2.5"));
            assert!(last.summary.ends_with("token fee 0"));
        }
    }

    #[test]
    fn the_compute_budget_instructions_are_there_at_any_price() {
        let route = Route::native(OWNER, PEER);
        for price in [0u64, 1, u64::MAX] {
            let instructions = route.instructions(1, units(500), price).expect("instructions");
            assert_eq!(instructions.len(), 3, "{price}");
            let programs: Vec<Pubkey> = instructions.iter().map(|entry| entry.program_id).collect();
            assert_eq!(
                programs,
                vec![COMPUTE_BUDGET_PROGRAM, COMPUTE_BUDGET_PROGRAM, SYSTEM_PROGRAM]
            );
        }
    }
}
