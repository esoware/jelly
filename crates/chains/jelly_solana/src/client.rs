use std::iter;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_net::WsSubscription;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::broadcast::{Attempt, BroadcastReply};
use crate::config::SolanaConfig;
use crate::cursor::PageRequest;
use crate::error::SolanaError;
use crate::method::Method;
use crate::node::{Answered, RpcCall, SolanaNode};
use crate::route::{Leg, Route};
use crate::rpc::{
    AccountQuery, BlockhashInfo, Commitment, CommitmentQuery, Contextual, Encoding, PageQuery,
    PrioritizationFee, RawAccount, SendQuery, SignatureRow, SignatureStatus, SimulateQuery,
    Simulation, StatusQuery, TransactionRow, TxQuery,
};
use crate::token::{Destination, SolanaToken};

const TARGET: &str = "jelly::solana::client";

/// A status query accepts at most this many signatures.
const MAX_STATUS_SIGNATURES: usize = 256;

/// For everything but history. `finalized` would add about 13 seconds to every number
/// on screen, and no `confirmed` block has ever been rolled back on mainnet.
const READ_COMMITMENT: Commitment = Commitment::Confirmed;

/// History is written to the database for good, so it's read only once rooted.
const HISTORY_COMMITMENT: Commitment = Commitment::Finalized;

/// Base64, so the build unpacks token account and mint state itself.
const ACCOUNT_QUERY: AccountQuery =
    AccountQuery { encoding: Encoding::Base64, commitment: READ_COMMITMENT };

/// Slots count confirmation depth and block height counts a blockhash's validity
/// window. Skipped slots make them diverge, so mixing them up would eventually
/// declare a live transaction dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tip {
    pub(crate) slot: u64,
    pub(crate) block_height: u64,
}

/// One signature's status as one endpoint saw it.
///
/// An endpoint behind the tip has no status for a transaction in a block it hasn't
/// reached, so its silence is judged against its own block height and never against
/// another endpoint's.
#[derive(Debug, Clone)]
pub(crate) struct StatusReading {
    pub(crate) status: Option<SignatureStatus>,
    pub(crate) block_height: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TokenHolding {
    pub(crate) token: SolanaToken,
    /// What the owner's associated account holds, or `None` when the owner has no
    /// account there.
    pub(crate) balance: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct Holdings {
    pub(crate) lamports: u64,
    pub(crate) tokens: Vec<TokenHolding>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Balances {
    pub(crate) lamports: u64,
    /// From the associated token account only, because that's the one a transfer
    /// spends from.
    pub(crate) token_spendable: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct BuildReads {
    pub(crate) blockhash: BlockhashInfo,
    pub(crate) balances: Balances,
    /// The program that owns the recipient's account, or `None` when it doesn't exist.
    /// A wallet is owned by the system program, and SOL sent to any other program's
    /// account can be lost.
    pub(crate) recipient_owner: Option<Pubkey>,
    /// Where a token transfer credits. `None` for a native transfer.
    pub(crate) destination: Option<Destination>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OptionalReads {
    pub(crate) account_rent_lamports: Option<u64>,
    pub(crate) token_account_rent_lamports: Option<u64>,
    pub(crate) fee_samples: Vec<PrioritizationFee>,
}

/// The adapter's JSON-RPC calls, decoded into this crate's own types.
#[derive(Debug)]
pub(crate) struct SolanaClient {
    node: Arc<dyn SolanaNode>,
    batch_size: usize,
}

impl SolanaClient {
    pub(crate) fn new(node: Arc<dyn SolanaNode>, config: &SolanaConfig) -> Self {
        Self { node, batch_size: config.batch_size() }
    }

    /// The slot and the block height, at the reading commitment.
    pub(crate) async fn tip(&self) -> Result<Tip, SolanaError> {
        let query = (CommitmentQuery { commitment: READ_COMMITMENT },);
        let calls =
            [RpcCall::new(Method::GetSlot, &query)?, RpcCall::new(Method::GetBlockHeight, &query)?];
        let answered = self.node.batch(&calls).await?;
        answered.check_count(Method::GetSlot, calls.len())?;
        Ok(Tip {
            slot: answered.decode_at(0, Method::GetSlot)?,
            block_height: answered.decode_at(1, Method::GetBlockHeight)?,
        })
    }

    /// One batch per group of owners, with each owner's balance and every associated
    /// token account read together.
    ///
    /// Token accounts are read by address, because public endpoints refuse
    /// `getTokenAccountsByOwner` without a key. Answers are read at a fixed stride, so a
    /// group always holds whole owners, even past the configured batch size.
    pub(crate) async fn holdings(&self, owners: &[Pubkey]) -> Result<Vec<Holdings>, SolanaError> {
        let stride = SolanaToken::all().count().saturating_add(1);
        let per_chunk = self.batch_size.checked_div(stride).unwrap_or(1).max(1);
        let mut holdings = Vec::with_capacity(owners.len());
        for group in owners.chunks(per_chunk) {
            let mut calls = Vec::with_capacity(group.len().saturating_mul(stride));
            for owner in group {
                calls.push(RpcCall::new(
                    Method::GetBalance,
                    &(owner.to_string(), CommitmentQuery { commitment: READ_COMMITMENT }),
                )?);
                for token in SolanaToken::all() {
                    calls.push(account_call(&token.account_of(owner))?);
                }
            }
            let answered = self.node.batch(&calls).await?;
            answered.check_count(Method::GetBalance, calls.len())?;
            for (owner, base) in group.iter().zip((0..calls.len()).step_by(stride)) {
                holdings.push(holdings_at(&answered, owner, base)?);
            }
        }
        Ok(holdings)
    }

    /// The blockhash, the payer's balances, and the accounts the transfer could credit,
    /// in one batch.
    ///
    /// Both the recipient and its associated token account are read, because the
    /// recipient may itself be the token account to credit. A Token-2022 mint is read
    /// too, and an unsupported configuration refuses the build.
    pub(crate) async fn build_reads(&self, route: &Route) -> Result<BuildReads, SolanaError> {
        let query = CommitmentQuery { commitment: READ_COMMITMENT };
        let mut calls = vec![
            RpcCall::new(Method::GetLatestBlockhash, &(query,))?,
            RpcCall::new(Method::GetBalance, &(route.payer.to_string(), query))?,
            account_call(&route.recipient)?,
        ];
        if let Leg::Token { token, source, destination } = route.leg {
            calls.push(account_call(&destination)?);
            calls.push(account_call(&source)?);
            if token.is_token_2022() {
                calls.push(account_call(&token.mint())?);
            }
        }
        let answered = self.node.batch(&calls).await?;
        answered.check_count(Method::GetLatestBlockhash, calls.len())?;

        let blockhash: Contextual<BlockhashInfo> =
            answered.decode_at(0, Method::GetLatestBlockhash)?;
        let lamports: Contextual<u64> = answered.decode_at(1, Method::GetBalance)?;
        let recipient: Contextual<Option<RawAccount>> =
            answered.decode_at(2, Method::GetAccountInfo)?;
        let (token_spendable, destination) = match route.leg {
            Leg::Native => (None, None),
            Leg::Token { token, destination: associated, .. } => {
                let associated_account: Contextual<Option<RawAccount>> =
                    answered.decode_at(3, Method::GetAccountInfo)?;
                let source: Contextual<Option<RawAccount>> =
                    answered.decode_at(4, Method::GetAccountInfo)?;
                if token.is_token_2022() {
                    let mint: Contextual<Option<RawAccount>> =
                        answered.decode_at(5, Method::GetAccountInfo)?;
                    token.validate_mint(
                        mint.value.as_ref().ok_or(SolanaError::TokenConfiguration)?,
                    )?;
                }
                let spendable = token.spendable_by(&route.payer, source.value.as_ref())?;
                let destination = token.destination(
                    route.recipient,
                    recipient.value.as_ref(),
                    associated,
                    associated_account.value.as_ref(),
                )?;
                (Some(spendable), Some(destination))
            }
        };
        Ok(BuildReads {
            blockhash: blockhash.value,
            balances: Balances { lamports: lamports.value, token_spendable },
            recipient_owner: recipient.value.map(|account| account.owner),
            destination,
        })
    }

    /// Reads a build can do without, so a failure only degrades the estimate. Without
    /// them, rent falls back to the compiled ceiling and every level prices at its floor.
    ///
    /// Fee samples are read even for a custom fee, because a quote prices every level
    /// beside the one the user chose.
    pub(crate) async fn optional_reads(&self, route: &Route) -> OptionalReads {
        let token_account_bytes = match route.leg {
            Leg::Native => None,
            Leg::Token { token, .. } => Some(token.account_bytes()),
        };
        let sizes: Vec<u64> = iter::once(0).chain(token_account_bytes).collect();
        let rent_calls = sizes
            .iter()
            .map(|bytes| RpcCall::new(Method::GetMinimumBalanceForRentExemption, &(bytes,)));
        let fee_call =
            RpcCall::new(Method::GetRecentPrioritizationFees, &(route.writable_accounts(),));
        let calls = match rent_calls.chain(iter::once(fee_call)).collect::<Result<Vec<_>, _>>() {
            Ok(calls) => calls,
            Err(error) => return degraded(&error),
        };

        let answered = match self.node.batch(&calls).await {
            Ok(answered) => answered,
            Err(error) => return degraded(&error),
        };
        let rent_at = |index: usize| {
            answered.decode_at::<u64>(index, Method::GetMinimumBalanceForRentExemption).ok()
        };
        let fee_samples = answered
            .decode_at::<Vec<PrioritizationFee>>(sizes.len(), Method::GetRecentPrioritizationFees)
            .unwrap_or_default();
        OptionalReads {
            account_rent_lamports: rent_at(0),
            token_account_rent_lamports: token_account_bytes.and_then(|_| rent_at(1)),
            fee_samples,
        }
    }

    /// Compute units consumed in simulation, or `None` when the simulation failed or
    /// reported zero. The node supplies the blockhash and skips signature checks.
    pub(crate) async fn simulate(
        &self,
        message: &VersionedMessage,
    ) -> Result<Option<u64>, SolanaError> {
        let encoded = BASE64.encode(unsigned_wire(message)?);
        let call = RpcCall::new(
            Method::SimulateTransaction,
            &(
                encoded,
                SimulateQuery {
                    encoding: Encoding::Base64,
                    sig_verify: false,
                    replace_recent_blockhash: true,
                    commitment: READ_COMMITMENT,
                },
            ),
        )?;
        let answered = self.node.call(&call).await?;
        let result: Contextual<Simulation> = answered.decode(Method::SimulateTransaction)?;
        if result.value.err.is_some() {
            return Ok(None);
        }
        Ok(result.value.units_consumed.filter(|units| *units > 0))
    }

    /// One page of signatures per request, in request order.
    ///
    /// `until` is applied locally, because a provider can reject it once it no longer
    /// indexes that transaction.
    pub(crate) async fn signature_pages(
        &self,
        requests: &[(&str, PageRequest)],
    ) -> Result<Vec<Vec<SignatureRow>>, SolanaError> {
        let mut pages = Vec::with_capacity(requests.len());
        for group in requests.chunks(self.batch_size) {
            let mut calls = Vec::with_capacity(group.len());
            for (address, page) in group {
                calls.push(RpcCall::new(
                    Method::GetSignaturesForAddress,
                    &(
                        address,
                        PageQuery {
                            limit: page.limit,
                            commitment: HISTORY_COMMITMENT,
                            before: page.before.as_deref(),
                        },
                    ),
                )?);
            }
            let answered = self.node.batch(&calls).await?;
            answered.check_count(Method::GetSignaturesForAddress, calls.len())?;
            for (index, (_, page)) in group.iter().enumerate() {
                let mut rows: Vec<SignatureRow> =
                    answered.decode_at(index, Method::GetSignaturesForAddress)?;
                if let Some(stop) = rows
                    .iter()
                    .position(|row| Some(row.signature.as_str()) == page.until.as_deref())
                {
                    rows.truncate(stop);
                }
                pages.push(rows);
            }
        }
        Ok(pages)
    }

    /// In request order. History is searched, because the recent-status cache doesn't
    /// reach a send from minutes ago.
    ///
    /// Each group shares a batch with a block height read, so every status arrives with
    /// the height of the endpoint that answered it. The height is asked first, so an
    /// endpoint that answers the two in turn has already reached that height when it
    /// reports the status.
    pub(crate) async fn statuses(
        &self,
        signatures: &[&str],
    ) -> Result<Vec<StatusReading>, SolanaError> {
        let mut found = Vec::with_capacity(signatures.len());
        for group in signatures.chunks(MAX_STATUS_SIGNATURES) {
            let calls = [
                RpcCall::new(
                    Method::GetBlockHeight,
                    &(CommitmentQuery { commitment: READ_COMMITMENT },),
                )?,
                RpcCall::new(
                    Method::GetSignatureStatuses,
                    &(group, StatusQuery { search_transaction_history: true }),
                )?,
            ];
            let answered = self.node.batch(&calls).await?;
            answered.check_count(Method::GetSignatureStatuses, calls.len())?;
            let block_height: u64 = answered.decode_at(0, Method::GetBlockHeight)?;
            let statuses: Contextual<Vec<Option<SignatureStatus>>> =
                answered.decode_at(1, Method::GetSignatureStatuses)?;
            if statuses.value.len() != group.len() {
                return Err(SolanaError::BatchLength {
                    endpoint: answered.endpoint().clone(),
                    method: Method::GetSignatureStatuses,
                    asked: group.len(),
                    answered: statuses.value.len(),
                });
            }
            found.extend(
                statuses.value.into_iter().map(|status| StatusReading { status, block_height }),
            );
        }
        Ok(found)
    }

    /// One attempt against one endpoint. After a transport failure, the next sync
    /// pass looks the signature up and resends only the identical signed bytes.
    pub(crate) async fn send_transaction(
        &self,
        raw: &[u8],
        attempt: Attempt,
    ) -> Result<BroadcastReply, SolanaError> {
        let call = RpcCall::new(
            Method::SendTransaction,
            &(
                BASE64.encode(raw),
                SendQuery {
                    encoding: Encoding::Base64,
                    skip_preflight: attempt == Attempt::Rebroadcast,
                    preflight_commitment: READ_COMMITMENT,
                },
            ),
        )?;
        self.node.broadcast(&call).await
    }

    /// One transaction, or `None` when the endpoint doesn't have it.
    ///
    /// Each is its own request, because public endpoints refuse a batch that carries
    /// more than one `getTransaction`.
    ///
    /// A v1 answer differs from a v0 one only by a `transactionConfig` member this build
    /// doesn't read and by naming every account inline. Its `meta.fee` already includes
    /// the priority fee.
    pub(crate) async fn transaction(
        &self,
        signature: &str,
    ) -> Result<Option<TransactionRow>, SolanaError> {
        let query = TxQuery {
            encoding: Encoding::JsonParsed,
            commitment: HISTORY_COMMITMENT,
            max_supported_transaction_version: 1,
        };
        let call = RpcCall::new(Method::GetTransaction, &(signature, query))?;
        self.node.call(&call).await?.decode(Method::GetTransaction)
    }
}

/// Wakes sync whenever `account` changes, with the encoding and commitment
/// [`SolanaClient`] reads it at.
pub(crate) fn account_subscription(account: &Pubkey) -> WsSubscription {
    WsSubscription::new("accountSubscribe", serde_json::json!([account.to_string(), ACCOUNT_QUERY]))
}

fn account_call(account: &Pubkey) -> Result<RpcCall, SolanaError> {
    RpcCall::new(Method::GetAccountInfo, &(account.to_string(), ACCOUNT_QUERY))
}

/// One owner's balances, from the answers [`SolanaClient::holdings`] lays out
/// from `base`.
fn holdings_at(answered: &Answered, owner: &Pubkey, base: usize) -> Result<Holdings, SolanaError> {
    let lamports: Contextual<u64> = answered.decode_at(base, Method::GetBalance)?;
    let mut tokens = Vec::new();
    for (offset, token) in SolanaToken::all().enumerate() {
        let at = base.saturating_add(offset).saturating_add(1);
        let account: Contextual<Option<RawAccount>> =
            answered.decode_at(at, Method::GetAccountInfo)?;
        let balance = token.balance_of(owner, account.value.as_ref())?;
        tokens.push(TokenHolding { token, balance });
    }
    Ok(Holdings { lamports: lamports.value, tokens })
}

/// The message with a placeholder signature, which is enough for a simulation that
/// skips signature checks.
fn unsigned_wire(message: &VersionedMessage) -> Result<Vec<u8>, SolanaError> {
    let transaction =
        VersionedTransaction { signatures: vec![Signature::default()], message: message.clone() };
    wincode::serialize(&transaction).map_err(|_| SolanaError::Encode { what: "simulation" })
}

fn degraded(error: &SolanaError) -> OptionalReads {
    tracing::debug!(
        target: TARGET,
        %error,
        "a rent or fee sample was unavailable; compiled figures are used and the preview says so"
    );
    OptionalReads::default()
}

#[cfg(test)]
mod tests {
    use solana_compute_budget_interface::ComputeBudgetInstruction;
    use solana_hash::Hash;
    use solana_message::v0;

    use super::*;
    use crate::fixtures::OWNER;

    #[test]
    fn a_simulation_payload_carries_one_placeholder_signature() {
        let instruction = ComputeBudgetInstruction::set_compute_unit_limit(1_000);
        let compiled = v0::Message::try_compile(&OWNER, &[instruction], &[], Hash::default())
            .expect("it compiles");
        let message = VersionedMessage::V0(compiled);
        let bytes = unsigned_wire(&message).expect("it serializes");
        // One signature in a shortvec, then the message.
        assert_eq!(bytes.first().copied(), Some(1));
        assert_eq!(bytes.get(1..65), Some(&[0u8; 64][..]));
        assert_eq!(bytes.get(65..), Some(message.serialize().as_slice()));
    }
}
