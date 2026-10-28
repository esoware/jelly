//! Reads a compiled message back for the preview, one row per instruction.
//!
//! Instruction data is decoded by the programs' interface crates, so a row names
//! an instruction only when its bytes decode as one. Anything else is shown with
//! its raw bytes.

use borsh::BorshDeserialize as _;
use jelly_core::Amount;
use solana_compute_budget_interface::{ComputeBudgetInstruction, ID as COMPUTE_BUDGET_PROGRAM};
use solana_message::compiled_instruction::CompiledInstruction;
use solana_message::v0;
use solana_pubkey::Pubkey;
use solana_system_interface::instruction::SystemInstruction;
use solana_system_interface::program::ID as SYSTEM_PROGRAM;
use spl_associated_token_account_interface::instruction::AssociatedTokenAccountInstruction;
use spl_associated_token_account_interface::program::ID as ASSOCIATED_TOKEN_PROGRAM;
use spl_token_2022_interface::ID as TOKEN_2022_PROGRAM;
use spl_token_2022_interface::extension::transfer_fee::instruction::TransferFeeInstruction;
use spl_token_2022_interface::instruction::TokenInstruction as Token2022Instruction;
use spl_token_interface::ID as TOKEN_PROGRAM;
use spl_token_interface::instruction::TokenInstruction;

use crate::fee::lamports_text;

/// One line of a decoded preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedInstruction {
    pub(crate) index: usize,
    pub(crate) program: &'static str,
    pub(crate) summary: String,
}

/// Decodes the compiled message rather than the caller's intent, so the user
/// reads what gets signed. An unrecognized instruction is reported, never
/// dropped.
pub(crate) fn decode_message(message: &v0::Message) -> Vec<DecodedInstruction> {
    message
        .instructions
        .iter()
        .enumerate()
        .map(|(index, instruction)| decode_one(index, instruction, &message.account_keys))
        .collect()
}

fn key_at(keys: &[Pubkey], index: u8) -> String {
    keys.get(usize::from(index)).map_or_else(|| format!("key {index}"), ToString::to_string)
}

fn decode_one(
    index: usize,
    instruction: &CompiledInstruction,
    keys: &[Pubkey],
) -> DecodedInstruction {
    let program_id = keys.get(usize::from(instruction.program_id_index)).copied();
    let accounts: Vec<String> = instruction.accounts.iter().map(|at| key_at(keys, *at)).collect();
    let data = instruction.data.as_slice();

    let (program, summary) = match program_id {
        Some(id) if id == SYSTEM_PROGRAM => ("System", decode_system(data, &accounts)),
        Some(id) if id == TOKEN_PROGRAM => ("Token program", decode_token(data, &accounts)),
        Some(id) if id == TOKEN_2022_PROGRAM => ("Token-2022", decode_token_2022(data, &accounts)),
        Some(id) if id == ASSOCIATED_TOKEN_PROGRAM => {
            ("Associated token account program", decode_associated_token(data, &accounts))
        }
        Some(id) if id == COMPUTE_BUDGET_PROGRAM => ("Compute budget", decode_compute_budget(data)),
        Some(id) => (
            "Unrecognized program",
            format!("{id}: data length {}, 0x{}", data.len(), hex::encode(data)),
        ),
        None => ("Unrecognized program", format!("no account key, 0x{}", hex::encode(data))),
    };
    DecodedInstruction { index, program, summary }
}

fn account(accounts: &[String], index: usize) -> &str {
    accounts.get(index).map_or("unknown", String::as_str)
}

/// Trailing bytes are ignored, as the system program ignores them.
fn decode_system(data: &[u8], accounts: &[String]) -> String {
    match wincode::deserialize::<SystemInstruction>(data) {
        Ok(SystemInstruction::Transfer { lamports }) => format!(
            "transfer {} from {} to {}",
            lamports_text(lamports),
            account(accounts, 0),
            account(accounts, 1)
        ),
        Ok(SystemInstruction::CreateAccount { .. }) => "create account".to_owned(),
        Ok(SystemInstruction::Allocate { .. }) => "allocate account space".to_owned(),
        Ok(_) => format!("system instruction 0x{}", hex::encode(data)),
        Err(_) => format!("unreadable system instruction 0x{}", hex::encode(data)),
    }
}

/// Trailing bytes are ignored, as the token program ignores them.
fn decode_token(data: &[u8], accounts: &[String]) -> String {
    match TokenInstruction::unpack(data) {
        Ok(TokenInstruction::TransferChecked { amount, decimals }) => {
            let shown = Amount::from(amount).format(decimals);
            format!(
                "checked transfer of {shown} ({amount} in the smallest unit, {decimals} decimals) \
                 of mint {} from {} to {}, authority {}",
                account(accounts, 1),
                account(accounts, 0),
                account(accounts, 2),
                account(accounts, 3)
            )
        }
        Ok(TokenInstruction::Transfer { amount }) => {
            format!("transfer, decimals unchecked, {amount} in the smallest unit")
        }
        Ok(TokenInstruction::CloseAccount) => "close account".to_owned(),
        Ok(_) => format!("token instruction 0x{}", hex::encode(data)),
        Err(_) => format!("unreadable token instruction 0x{}", hex::encode(data)),
    }
}

/// Token-2022 shares the classic layout, so anything but its fee-carrying
/// transfer reads as a classic instruction.
fn decode_token_2022(data: &[u8], accounts: &[String]) -> String {
    if Token2022Instruction::unpack(data) == Ok(Token2022Instruction::TransferFeeExtension)
        && let Some(TransferFeeInstruction::TransferCheckedWithFee { amount, decimals, fee }) =
            data.get(1..).and_then(|rest| TransferFeeInstruction::unpack(rest).ok())
    {
        let shown = Amount::from(amount).format(decimals);
        let fee = Amount::from(fee).format(decimals);
        return format!(
            "checked transfer of {shown} ({amount} in the smallest unit, {decimals} decimals) \
             of mint {} from {} to {}, authority {}, token fee {fee}",
            account(accounts, 1),
            account(accounts, 0),
            account(accounts, 2),
            account(accounts, 3)
        );
    }
    decode_token(data, accounts)
}

/// A trailing byte is unreadable, because the program refuses one.
fn decode_associated_token(data: &[u8], accounts: &[String]) -> String {
    // Empty data is the original encoding of Create.
    let instruction = if data.is_empty() {
        Ok(AssociatedTokenAccountInstruction::Create)
    } else {
        AssociatedTokenAccountInstruction::try_from_slice(data)
    };
    let target = account(accounts, 1);
    let owner = account(accounts, 2);
    let mint = account(accounts, 3);
    match instruction {
        Ok(AssociatedTokenAccountInstruction::Create) => {
            format!("create token account {target} for owner {owner}, mint {mint}")
        }
        Ok(AssociatedTokenAccountInstruction::CreateIdempotent) => format!(
            "create token account {target} for owner {owner}, mint {mint}, a no-op if it already \
             exists"
        ),
        Ok(AssociatedTokenAccountInstruction::RecoverNested) => {
            format!("associated token account instruction 0x{}", hex::encode(data))
        }
        Err(_) => {
            format!("unreadable associated token account instruction 0x{}", hex::encode(data))
        }
    }
}

/// Trailing bytes are ignored, as the runtime ignores them.
fn decode_compute_budget(data: &[u8]) -> String {
    let mut rest = data;
    match ComputeBudgetInstruction::deserialize(&mut rest) {
        Ok(ComputeBudgetInstruction::SetComputeUnitLimit(units)) => {
            format!("set compute unit limit to {units}")
        }
        Ok(ComputeBudgetInstruction::SetComputeUnitPrice(price)) => {
            format!("set compute unit price to {price} micro-lamports per compute unit")
        }
        Ok(ComputeBudgetInstruction::RequestHeapFrame(bytes)) => {
            format!("request a heap frame of {bytes} bytes")
        }
        Ok(ComputeBudgetInstruction::SetLoadedAccountsDataSizeLimit(bytes)) => {
            format!("set loaded accounts data size limit to {bytes} bytes")
        }
        Ok(ComputeBudgetInstruction::Unused) | Err(_) => {
            format!("unreadable compute budget instruction 0x{}", hex::encode(data))
        }
    }
}

#[cfg(test)]
mod tests {
    use solana_hash::Hash;
    use solana_instruction::Instruction;
    use solana_system_interface::instruction as system_instruction;
    use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
    use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
    use spl_token_interface::instruction::transfer_checked;

    use super::*;
    use crate::fixtures::{OWNER, PEER};

    const MINT: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    const ONE_SOL: &str = "0200000000ca9a3b00000000";

    fn summaries(instructions: &[Instruction]) -> Vec<(&'static str, String)> {
        let message = v0::Message::try_compile(&OWNER, instructions, &[], Hash::default())
            .expect("it compiles");
        decode_message(&message).into_iter().map(|row| (row.program, row.summary)).collect()
    }

    #[test]
    fn every_instruction_a_transfer_writes_reads_back_with_its_accounts() {
        let source = get_associated_token_address_with_program_id(&OWNER, &MINT, &TOKEN_PROGRAM);
        let destination =
            get_associated_token_address_with_program_id(&PEER, &MINT, &TOKEN_PROGRAM);
        let rows = summaries(&[
            ComputeBudgetInstruction::set_compute_unit_limit(45_000),
            ComputeBudgetInstruction::set_compute_unit_price(1_000),
            system_instruction::transfer(&OWNER, &PEER, 1_000_000),
            create_associated_token_account_idempotent(&OWNER, &PEER, &MINT, &TOKEN_PROGRAM),
            transfer_checked(
                &TOKEN_PROGRAM,
                &source,
                &MINT,
                &destination,
                &OWNER,
                &[],
                1_500_000,
                6,
            )
            .expect("the classic token program"),
        ]);
        assert_eq!(
            rows,
            vec![
                ("Compute budget", "set compute unit limit to 45000".to_owned()),
                (
                    "Compute budget",
                    "set compute unit price to 1000 micro-lamports per compute unit".to_owned()
                ),
                ("System", format!("transfer {} from {OWNER} to {PEER}", lamports_text(1_000_000))),
                (
                    "Associated token account program",
                    format!(
                        "create token account {destination} for owner {PEER}, mint {MINT}, a \
                         no-op if it already exists"
                    )
                ),
                (
                    "Token program",
                    format!(
                        "checked transfer of 1.5 (1500000 in the smallest unit, 6 decimals) of \
                         mint {MINT} from {source} to {destination}, authority {OWNER}"
                    )
                ),
            ]
        );
    }

    #[test]
    fn a_token_2022_instruction_is_read_with_the_classic_layout_and_labeled_apart() {
        let keys = [OWNER, MINT, PEER, TOKEN_2022_PROGRAM];
        let instruction = CompiledInstruction {
            program_id_index: 3,
            accounts: vec![0, 1, 2, 0],
            data: hex::decode("0c60e316000000000006").expect("hex"),
        };
        let decoded = decode_one(0, &instruction, &keys);
        assert_eq!(decoded.program, "Token-2022");
        assert!(decoded.summary.starts_with("checked transfer of 1.5 "), "{}", decoded.summary);

        let fee_transfer = CompiledInstruction {
            data: hex::decode("1a0160e3160000000000060000000000000000").expect("hex"),
            ..instruction.clone()
        };
        assert_eq!(
            decode_one(0, &fee_transfer, &keys).summary,
            format!(
                "checked transfer of 1.5 (1500000 in the smallest unit, 6 decimals) of mint \
                 {MINT} from {OWNER} to {PEER}, authority {OWNER}, token fee 0"
            )
        );

        let extension = CompiledInstruction { data: vec![26, 1], ..instruction };
        let decoded = decode_one(0, &extension, &keys);
        assert_eq!(decoded.summary, "unreadable token instruction 0x1a01");
    }

    #[test]
    fn an_unrecognized_program_is_reported_with_its_bytes() {
        let instruction =
            CompiledInstruction { program_id_index: 1, accounts: vec![], data: vec![7] };
        let decoded = decode_one(0, &instruction, &[OWNER, PEER]);
        assert_eq!(decoded.program, "Unrecognized program");
        assert_eq!(decoded.summary, format!("{PEER}: data length 1, 0x07"));
        let missing =
            decode_one(0, &CompiledInstruction { program_id_index: 9, ..instruction }, &[]);
        assert_eq!(missing.summary, "no account key, 0x07");
    }

    #[test]
    fn a_missing_account_reads_as_unknown_rather_than_as_another_key() {
        let summary = decode_system(&hex::decode(ONE_SOL).expect("hex"), &[]);
        assert_eq!(
            summary,
            format!("transfer {} from unknown to unknown", lamports_text(1_000_000_000))
        );
    }

    #[test]
    fn a_truncated_instruction_is_reported_rather_than_read_past_its_end() {
        assert_eq!(decode_system(&[2, 0, 0, 0], &[]), "unreadable system instruction 0x02000000");
        assert_eq!(decode_token(&[12, 1], &[]), "unreadable token instruction 0x0c01");
        assert_eq!(decode_compute_budget(&[2]), "unreadable compute budget instruction 0x02");
        assert_eq!(decode_compute_budget(&[]), "unreadable compute budget instruction 0x");
        assert_eq!(decode_token(&[], &[]), "unreadable token instruction 0x");
    }

    #[test]
    fn an_unrecognized_instruction_is_still_reported() {
        let summary = decode_system(&[99, 0, 0, 0, 1], &[]);
        assert_eq!(summary, "unreadable system instruction 0x6300000001");
        assert_eq!(decode_token(&[200], &[]), "unreadable token instruction 0xc8");
        assert_eq!(decode_compute_budget(&[0]), "unreadable compute budget instruction 0x00");
        assert_eq!(
            decode_associated_token(&[3], &[]),
            "unreadable associated token account instruction 0x03"
        );
        assert_eq!(
            decode_token(&[4, 1, 0, 0, 0, 0, 0, 0, 0], &[]),
            "token instruction 0x040100000000000000"
        );
    }

    /// Each reader is as lenient about trailing bytes as the program it stands
    /// for, so a row never names an instruction the program would refuse, nor
    /// hides one it would run.
    #[test]
    fn trailing_bytes_are_read_the_way_each_program_reads_them() {
        assert_eq!(
            decode_compute_budget(&[2, 0xc8, 0xaf, 0, 0, 0xff]),
            "set compute unit limit to 45000"
        );
        assert_eq!(decode_token(&[9, 0xff], &[]), "close account");
        let mut transfer = hex::decode(ONE_SOL).expect("hex");
        transfer.push(0xff);
        assert!(decode_system(&transfer, &[]).starts_with("transfer 1 SOL"));
        assert_eq!(
            decode_associated_token(&[1, 0], &[]),
            "unreadable associated token account instruction 0x0100"
        );
        assert!(decode_associated_token(&[], &[]).starts_with("create token account unknown"));
    }
}
