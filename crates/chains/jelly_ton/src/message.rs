use tonlib_core::cell::{ArcCell, Cell, CellBuilder};
use tonlib_core::message::{CommonMsgInfo, InternalMessage, TonMessage as _, TransferMessage};
use tonlib_core::tlb_types::primitives::either::{EitherRef, EitherRefLayout};
use tonlib_core::tlb_types::tep::jetton::JettonTransferMessage;
use tonlib_core::tlb_types::tlb::TLB as _;
use tonlib_core::types::TonAddress;
use tonlib_core::wallet::versioned::v3::WalletExtMsgBodyV3;
use tonlib_core::wallet::versioned::v4::WalletExtMsgBodyV4;
use tonlib_core::wallet::versioned::v5::WalletExtMsgBodyV5;

use crate::address::{TonDestination, ton_destination};
use crate::error::TonError;
use crate::payload::{INVALID_PAYMENT, Payment, Transfer};
use crate::wallet::{Version, ton_wallet};

const SEND_PAY_FEES_SEPARATELY: u8 = 1;
const SEND_IGNORE_ERRORS: u8 = 2;
const SEND_CARRY_ALL_BALANCE: u8 = 128;
/// The least a transfer can forward and still have TEP-74 notify the recipient.
const FORWARD_NANOGRAM: u32 = 1;
/// A text comment's root cell holds this many bytes after its 32-bit zero opcode, and
/// each cell it references holds `COMMENT_TAIL_BYTES`.
const COMMENT_HEAD_BYTES: usize = 123;
const COMMENT_TAIL_BYTES: usize = 127;

/// The unsigned body of the external message `payment` sends, in the layout of the
/// wallet's version.
pub(crate) fn body(payment: &Payment) -> Result<Cell, TonError> {
    let wallet = ton_wallet(&payment.pubkey, payment.version)?;
    let (message, mode) = internal_message(payment)?;
    let cell = match payment.version {
        Version::V5r1 => WalletExtMsgBodyV5 {
            wallet_id: wallet.wallet_id,
            valid_until: payment.valid_until,
            msg_seqno: payment.seqno,
            msgs_modes: vec![mode],
            msgs: vec![message],
        }
        .to_cell()?,
        Version::V4r2 => WalletExtMsgBodyV4 {
            subwallet_id: wallet.wallet_id,
            valid_until: payment.valid_until,
            msg_seqno: payment.seqno,
            opcode: 0,
            msgs_modes: vec![mode],
            msgs: vec![message],
        }
        .to_cell()?,
        Version::V3r2 => WalletExtMsgBodyV3 {
            subwallet_id: wallet.wallet_id,
            valid_until: payment.valid_until,
            msg_seqno: payment.seqno,
            msgs_modes: vec![mode],
            msgs: vec![message],
        }
        .to_cell()?,
    };
    Ok(cell)
}

/// The one internal message the wallet sends, with the mode it sends it in.
fn internal_message(payment: &Payment) -> Result<(ArcCell, u8), TonError> {
    let TonDestination { address: to, .. } =
        ton_destination(&payment.to).map_err(|_| INVALID_PAYMENT)?;
    let (target, is_bounceable, value, mode, body) = match payment.transfer {
        Transfer::Native { amount_nanogram, is_bounceable, ref comment } => {
            // A message sent with `SEND_CARRY_ALL_BALANCE` carries the whole remaining
            // balance and ignores its value.
            let (value, mode) = if payment.is_max {
                (0, SEND_CARRY_ALL_BALANCE | SEND_IGNORE_ERRORS)
            } else {
                (amount_nanogram, SEND_PAY_FEES_SEPARATELY | SEND_IGNORE_ERRORS)
            };
            (to, is_bounceable, value, mode, comment_body(comment.as_deref())?)
        }
        Transfer::Jetton {
            ref wallet, amount, query_id, attached_nanogram, ref comment, ..
        } => {
            let from = ton_destination(&payment.from).map_err(|_| INVALID_PAYMENT)?.address;
            // The forward payload goes inline when the comment fits in the body and in a
            // reference otherwise, as MyTonWallet writes it. No comment is an empty inline
            // payload, a single zero bit.
            let body = JettonTransferMessage {
                query_id,
                amount: amount.base_units().into(),
                destination: to.to_msg_address(),
                response_destination: from.to_msg_address(),
                custom_payload: None,
                forward_ton_amount: FORWARD_NANOGRAM.into(),
                forward_payload: EitherRef {
                    value: comment_body(comment.as_deref())?.to_arc(),
                    layout: EitherRefLayout::Native,
                },
            }
            .to_cell()?;
            let wallet = TonAddress::from_hex_str(wallet).map_err(|_| INVALID_PAYMENT)?;
            let mode = SEND_PAY_FEES_SEPARATELY | SEND_IGNORE_ERRORS;
            (wallet, true, attached_nanogram, mode, body)
        }
    };
    let message = TransferMessage::new(
        CommonMsgInfo::InternalMessage(InternalMessage {
            ihr_disabled: true,
            bounce: is_bounceable,
            bounced: false,
            src: TonAddress::NULL,
            dest: target,
            value: value.into(),
            ihr_fee: 0u32.into(),
            fwd_fee: 0u32.into(),
            created_lt: 0,
            created_at: 0,
        }),
        body.to_arc(),
    )
    .build()?;
    Ok((message.to_arc(), mode))
}

/// A text comment is a snake: a 32-bit zero opcode and its head in the root cell, then
/// the rest in a chain of referenced cells. No comment is an empty body.
fn comment_body(comment: Option<&str>) -> Result<Cell, TonError> {
    let mut body = CellBuilder::new();
    if let Some(comment) = comment {
        let (head, rest) = comment.as_bytes().split_at(comment.len().min(COMMENT_HEAD_BYTES));
        let mut tail = None;
        for chunk in rest.chunks(COMMENT_TAIL_BYTES).rev() {
            let mut builder = CellBuilder::new();
            builder.store_slice(chunk)?;
            if let Some(next) = tail {
                builder.store_reference(&next)?;
            }
            tail = Some(builder.build()?.to_arc());
        }
        body.store_u32(32, 0)?;
        body.store_slice(head)?;
        if let Some(tail) = tail {
            body.store_reference(&tail)?;
        }
    }
    Ok(body.build()?)
}
