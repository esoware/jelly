use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use tonlib_core::cell::dict::predefined_readers::{key_reader_u16, val_reader_ref_cell};
use tonlib_core::cell::{ArcCell, Cell, CellParser};
use tonlib_core::tlb_types::block::coins::Grams;
use tonlib_core::tlb_types::block::message::{CommonMsgInfo, Message};
use tonlib_core::tlb_types::block::msg_address::MsgAddress;
use tonlib_core::tlb_types::tlb::TLB as _;
use tonlib_core::types::TonAddress;

use crate::client::Position;
use crate::error::TonError;
use crate::sign::normalized_hash;

#[derive(Debug, Deserialize)]
pub(crate) struct AccountTx {
    utime: u32,
    data: String,
    pub(crate) transaction_id: Position,
}

/// One transaction of one account, read from its cell rather than from the provider's
/// summary of it.
#[derive(Debug, Clone)]
pub(crate) struct Tx {
    pub(crate) lt: u64,
    pub(crate) previous: Position,
    pub(crate) utime: u32,
    /// The transaction's hash, except that an external-in transaction is keyed by its
    /// normalized message hash, so a send keeps the txid that signing returned.
    pub(crate) txid: String,
    /// `total_fees`, the share of the fees collected in this transaction.
    pub(crate) fee_nanogram: u64,
    pub(crate) inbound: Inbound,
    pub(crate) outbound: Vec<Outbound>,
    pub(crate) phases: Phases,
}

#[derive(Debug, Clone)]
pub(crate) enum Inbound {
    External,
    Internal { from: TonAddress, value_nanogram: u64, is_bounced: bool, body: ArcCell },
    None,
}

#[derive(Debug, Clone)]
pub(crate) struct Outbound {
    pub(crate) to: TonAddress,
    pub(crate) value_nanogram: u64,
    /// `fwd_fee` and `ihr_fee`, which the sender pays beyond `total_fees`.
    pub(crate) fee_nanogram: u64,
    pub(crate) is_bounceable: bool,
    pub(crate) body: ArcCell,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Phases {
    pub(crate) is_aborted: bool,
    pub(crate) is_skipped: bool,
    pub(crate) is_bounced: bool,
}

impl AccountTx {
    /// Reads `data` as block.tlb's `transaction$0111 account_addr lt prev_trans_hash
    /// prev_trans_lt now outmsg_cnt:uint15 orig_status end_status ^[in_msg out_msgs]
    /// total_fees state_update:^ description:^`, from
    /// <https://github.com/ton-blockchain/ton/blob/master/crypto/block/block.tlb>.
    pub(crate) fn read(&self, account: &TonAddress) -> Result<Tx, TonError> {
        let cell = Cell::from_boc(&STANDARD.decode(&self.data).map_err(|_| TonError::Protocol)?)?;
        let mut parser = cell.parser();
        if parser.load_u8(4)? != 7
            || parser.load_tonhash()? != account.hash_part
            || parser.load_u64(64)? != self.transaction_id.lt
            || cell.cell_hash() != self.transaction_id.hash.into()
        {
            return Err(TonError::Protocol);
        }
        let hash = parser.load_tonhash()?.into();
        let previous_lt = parser.load_u64(64)?;
        if previous_lt >= self.transaction_id.lt || parser.load_u32(32)? != self.utime {
            return Err(TonError::Protocol);
        }
        let outgoing_count = parser.load_u16(15)?;
        parser.load_u8(4)?;
        let messages = parser.next_reference()?;
        let fee_nanogram = u64::try_from(parser.load_coins()?).map_err(|_| TonError::Protocol)?;
        if parser.load_bit()? {
            parser.next_reference()?;
        }
        parser.next_reference()?;
        let description = parser.next_reference()?;
        parser.ensure_empty()?;
        let mut messages = messages.parser();
        let incoming =
            messages.load_maybe_cell_ref()?.map(|cell| Message::from_cell(&cell)).transpose()?;
        let outgoing = messages.load_dict(15, key_reader_u16, val_reader_ref_cell)?;
        if outgoing.len() != usize::from(outgoing_count) {
            return Err(TonError::Protocol);
        }
        messages.ensure_empty()?;
        let mut indexed: Vec<(u16, ArcCell)> = outgoing.into_iter().collect();
        indexed.sort_by_key(|(index, _)| *index);
        let mut txid = hex::encode(self.transaction_id.hash);
        let inbound = match incoming {
            None => Inbound::None,
            Some(message) => match message.info {
                CommonMsgInfo::ExtIn(info) => {
                    if message_address(&info.dest)? != *account {
                        return Err(TonError::Protocol);
                    }
                    txid = normalized_hash(account, &message.body.value)?;
                    Inbound::External
                }
                CommonMsgInfo::Int(info) => {
                    if message_address(&info.dest)? != *account {
                        return Err(TonError::Protocol);
                    }
                    Inbound::Internal {
                        from: message_address(&info.src)?,
                        value_nanogram: grams_nanogram(&info.value.grams)?,
                        is_bounced: info.bounced,
                        body: message.body.value,
                    }
                }
                CommonMsgInfo::ExtOut(_) => return Err(TonError::Protocol),
            },
        };
        let mut outbound = Vec::with_capacity(indexed.len());
        for (_, cell) in indexed {
            let message = Message::from_cell(&cell)?;
            if let CommonMsgInfo::Int(info) = message.info {
                if message_address(&info.src)? != *account {
                    return Err(TonError::Protocol);
                }
                outbound.push(Outbound {
                    to: message_address(&info.dest)?,
                    value_nanogram: grams_nanogram(&info.value.grams)?,
                    fee_nanogram: grams_nanogram(&info.fwd_fee)?
                        .checked_add(grams_nanogram(&info.ihr_fee)?)
                        .ok_or(TonError::Protocol)?,
                    is_bounceable: info.bounce,
                    body: message.body.value,
                });
            }
        }
        Ok(Tx {
            lt: self.transaction_id.lt,
            previous: Position { lt: previous_lt, hash },
            utime: self.utime,
            txid,
            fee_nanogram,
            inbound,
            outbound,
            phases: phases(&mut description.parser())?,
        })
    }
}

fn message_address<T: Clone + Into<MsgAddress>>(address: &T) -> Result<TonAddress, TonError> {
    TonAddress::from_msg_address(address.clone()).map_err(|_| TonError::Protocol)
}

fn grams_nanogram(grams: &Grams) -> Result<u64, TonError> {
    u64::try_from(&grams.amount).map_err(|_| TonError::Protocol)
}

/// Reads an ordinary `trans_ord$0000` description from block.tlb through its bounce phase.
/// Other description kinds count as neither aborted, skipped, nor bounced.
fn phases(parser: &mut CellParser<'_>) -> Result<Phases, TonError> {
    if parser.load_u8(4)? != 0 {
        return Ok(Phases::default());
    }
    parser.load_bit()?;
    if parser.load_bit()? {
        parser.load_coins()?;
        if parser.load_bit()? {
            parser.load_coins()?;
        }
        if parser.load_bit()? {
            parser.load_bit()?;
        }
    }
    if parser.load_bit()? {
        if parser.load_bit()? {
            parser.load_coins()?;
        }
        parser.load_coins()?;
        if parser.load_bit()? {
            parser.next_reference()?;
        }
    }
    if parser.load_bit()? {
        parser.load_u8(3)?;
        parser.load_coins()?;
        parser.next_reference()?;
    } else if parser.load_u8(2)? == 3 {
        parser.load_bit()?;
    }
    let is_skipped = match parser.load_maybe_cell_ref()? {
        Some(action) => {
            let mut action = action.parser();
            action.load_u8(3)?;
            if action.load_bit()? {
                action.load_bit()?;
            }
            if action.load_bit()? {
                action.load_coins()?;
            }
            if action.load_bit()? {
                action.load_coins()?;
            }
            action.load_i32(32)?;
            if action.load_bit()? {
                action.load_i32(32)?;
            }
            action.load_u16(16)?;
            action.load_u16(16)?;
            // With mode 2, a message that fails is skipped instead of aborting the
            // transaction, so a send that skipped an action and created no message failed.
            action.load_u16(16)? > 0 && action.load_u16(16)? == 0
        }
        None => false,
    };
    let is_aborted = parser.load_bit()?;
    // Only `tr_phase_bounce_ok` starts with a set bit, and only it sends the inbound
    // value back. A bounce that can't pay for its message leaves the value credited.
    let is_bounced = parser.load_bit()? && parser.load_bit()?;
    Ok(Phases { is_aborted, is_skipped, is_bounced })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{address, rows};

    const OWNER: &str = "UQDv2YSmlrlLH3hLNOVxC8FcQf4F9eGNs4vb2zKma4txo6i3";

    fn history() -> Vec<AccountTx> {
        rows(include_str!("../tests/fixtures/history.json"))
    }

    #[test]
    fn a_row_whose_lt_disagrees_with_its_cell_is_refused() {
        let mut forged = history().into_iter().next().unwrap();
        forged.transaction_id.lt = 1;
        let error = forged.read(&address(OWNER)).unwrap_err();
        assert!(matches!(error, TonError::Protocol), "{error:?}");
    }

    #[test]
    fn a_row_of_another_account_is_refused() {
        let row = history().into_iter().next().unwrap();
        let error = row.read(&address("EQB61bsO2ekfVGrBkk8o8HWqoQkdbht3q2B9VywE4dnv-5Ko"));
        assert!(matches!(error, Err(TonError::Protocol)), "{error:?}");
    }

    #[test]
    fn a_bounced_return_reads_its_flag_and_its_sender() {
        // A real mainnet transaction of a V4R2 wallet, the return of a USDT transfer
        // its jetton wallet refused.
        let rows = rows(include_str!("../tests/fixtures/usdt_bounce_owner.json"));
        let owner = address("0:6B0D3FA7A07B0C116C947436FB46CB269C1E9FC4BA113001BF72C060404041A5");
        let tx = rows.first().unwrap().read(&owner).unwrap();
        let Inbound::Internal { from, value_nanogram, is_bounced, .. } = tx.inbound else {
            panic!("expected an internal message, got {:?}", tx.inbound);
        };
        assert!(is_bounced);
        assert_eq!(value_nanogram, 49_866_665);
        assert_eq!(from, address("EQAmBS_chf1_o3vUsWgn9S-xV1ACmRlDIX6qb9eLVJw8rZaB"));
        assert!(tx.outbound.is_empty());
        let send = rows[1].read(&owner).unwrap();
        assert!(matches!(send.inbound, Inbound::External));
        assert_eq!(send.outbound.len(), 1);
        assert!(send.outbound[0].is_bounceable);
        assert_eq!(send.outbound[0].value_nanogram, 50_000_000);
    }
}
