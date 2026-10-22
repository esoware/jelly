//! No test in this crate opens a socket.
//!
//! The seam is [`MoneroNode`], and everything above it is the shipped
//! [`MoneroAdapter`], including the scan, the reorganisation walk, ring selection,
//! signing, and in-flight follow-up.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::io;
use std::ops::RangeInclusive;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainId, ChainSyncProgress, Clock, CursorStore,
    EndpointLabel, FailureKind, FeeSelection, KeyKind, KeyMaterial, Rejection, SignedTx, TxDetail,
    TxRequest, UnsignedTx, WalletEvent, WatchedAddress,
    test_util::{
        FakeClock, FakeEventSink, MemoryCursorStore, TxRequestBuilder, WatchedAddressBuilder,
    },
};
use jelly_keypair::{PathTemplate, test_util::test_keychain};
use jelly_monero::{
    BroadcastReply, Distribution, FeeLadder, MoneroAccount, MoneroAdapter, MoneroConfig,
    MoneroError, MoneroKeys, MoneroNode, SPEC,
};
use jelly_net::NetError;
use monero_address::SubaddressIndex;
use monero_interface::ScannableBlock;
use monero_wallet::block::{Block, BlockHeader};
use monero_wallet::ed25519::{CompressedPoint, Point, Scalar};
use monero_wallet::ringct::{PrunedRctProofs, RctBase, RctType};
use monero_wallet::transaction::{
    Input, NotPruned, Pruned, Timelock, Transaction, TransactionPrefix,
};
use monero_wallet::{Scanner, ViewPair, WalletOutput};
use parking_lot::Mutex;
use serde::Deserialize;
use zeroize::Zeroizing;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error>>;

/// The endpoint the fake answers as. Two operations are pinned to an endpoint, so
/// a test can prove one transaction's whole ring came from one node.
pub(crate) const ENDPOINT: &str = "https://node.invalid:18081";

/// How [`ENDPOINT`] appears in a failure.
fn endpoint_label() -> EndpointLabel {
    EndpointLabel::new("https", "node.invalid", Some(18_081))
}

/// Blocks in the fake's `RingCT` output distribution, and outputs per block.
const DISTRIBUTION_BLOCKS: u64 = 8_000;
const OUTPUTS_PER_BLOCK: u64 = 4;

/// Empty blocks the harness serves after the recorded one.
///
/// Decoy selection samples an output age from a gamma distribution with a median
/// of about two days and rejects a sample past the newest spendable output, so a
/// chain has to span more than that for a ring to be selectable at all. The
/// recorded block is near the genesis of its chain, so everything above it is
/// synthetic.
pub(crate) const CHAIN_BLOCKS: u64 = 3_000;

/// The per-weight rates the fake quotes for the three tiers the levels pay, which are
/// mainnet's multipliers.
pub(crate) const RATES: [u64; 3] = [1, 4, 16];

/// Ring size for CLSAG with Bulletproofs+, fixed by consensus since hardfork 15.
const RING_LEN: u8 = 16;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Recorded {
    pub(crate) spend_key: String,
    pub(crate) view_key: String,
    pub(crate) block: String,
    pub(crate) pruned_transactions: Vec<String>,
    pub(crate) output_index_for_first_ringct_output: Option<u64>,
    pub(crate) tx_hash: String,
    pub(crate) output_amount: u64,
}

pub(crate) fn recorded() -> serde_json::Result<Recorded> {
    serde_json::from_str(include_str!("../../fixtures/test_chain_block.json"))
}

/// The spend key of HD account `index` of the all-abandon phrase, at the spec's
/// default path.
pub(crate) fn hd_account(index: u32) -> Fallible<KeyMaterial> {
    let path = PathTemplate::parse(SPEC.default_path)?.path_at(index)?;
    Ok(test_keychain().key_material_at(SPEC.key_kind, &path)?)
}

pub(crate) fn bytes32(value: &str) -> Fallible<[u8; 32]> {
    Ok(<[u8; 32]>::try_from(hex::decode(value)?.as_slice())?)
}

/// The account the fixture's outputs were paid to.
pub(crate) fn account(recorded: &Recorded) -> Fallible<MoneroAccount> {
    Ok(MoneroAccount::try_from(MoneroKeys::from_spend_bytes(&bytes32(&recorded.spend_key)?))?)
}

/// An account no test wallet holds, to pay or to unlock as.
pub(crate) fn stranger() -> Result<MoneroAccount, MoneroError> {
    MoneroAccount::try_from(MoneroKeys::from_spend_bytes(&[0x42; 32]))
}

pub(crate) fn recorded_block(recorded: &Recorded) -> Fallible<ScannableBlock> {
    let block = Block::read(&mut hex::decode(&recorded.block)?.as_slice())?;
    let transactions = recorded
        .pruned_transactions
        .iter()
        .map(|raw| {
            Transaction::<Pruned>::read(&mut hex::decode(raw)?.as_slice()).map_err(Into::into)
        })
        .collect::<Fallible<_>>()?;
    Ok(ScannableBlock {
        block,
        transactions,
        output_index_for_first_ringct_output: recorded.output_index_for_first_ringct_output,
    })
}

pub(crate) fn recorded_height(recorded: &Recorded) -> Fallible<u64> {
    Ok(u64::try_from(recorded_block(recorded)?.block.number())?)
}

/// The outputs the fixture's account owns in the fixture's block, found by the
/// library's own scanner.
pub(crate) fn scanned(recorded: &Recorded) -> Fallible<Vec<WalletOutput>> {
    owned_in(recorded, recorded_block(recorded)?, &[])
}

/// The outputs of `block` the fixture's account owns at its primary address and at
/// `subaddresses`, found by the library's own scanner.
pub(crate) fn owned_in(
    recorded: &Recorded,
    block: ScannableBlock,
    subaddresses: &[SubaddressIndex],
) -> Fallible<Vec<WalletOutput>> {
    let spend = CompressedPoint::from(account(recorded)?.public_spend())
        .decompress()
        .ok_or("the fixture's public spend key is not a curve point")?;
    let view = Zeroizing::new(Scalar::read(&mut bytes32(&recorded.view_key)?.as_slice())?);
    let view_pair =
        ViewPair::new(spend, view).map_err(|_| "the fixture's keys are not a usable view pair")?;
    let mut scanner = Scanner::new(view_pair);
    for &index in subaddresses {
        scanner.register_subaddress(index);
    }
    let scanned = scanner.scan(block).map_err(|_| "the block does not scan")?;
    Ok(scanned.ignore_additional_timelock())
}

/// A point nobody holds the key to, for a decoy or a key image the fake serves.
///
/// `biased_hash` clears the cofactor, so the result is torsion free and never the
/// identity, which is what the library requires of a ring member.
pub(crate) fn unowned_point(tag: u8, index: u64) -> Point {
    let mut seed = [tag; 32];
    for (slot, byte) in seed.iter_mut().zip(index.to_le_bytes()) {
        *slot = byte;
    }
    Point::biased_hash(seed)
}

/// Builds a chain of blocks that link to each other, so the adapter's
/// reorganisation check has something real to check.
///
/// A block that cannot be built is reported by [`ChainBuilder::finish`], so the
/// steps chain without a check each.
#[derive(Debug)]
pub(crate) struct ChainBuilder {
    previous: [u8; 32],
    blocks: Vec<ScannableBlock>,
    unbuilt: Option<&'static str>,
}

impl ChainBuilder {
    pub(crate) const fn new(previous: [u8; 32]) -> Self {
        Self { previous, blocks: Vec::new(), unbuilt: None }
    }

    /// Appends a block from a fixture as it stands, adopting its hash as the
    /// parent of whatever follows.
    #[must_use]
    pub(crate) fn recorded(mut self, block: ScannableBlock) -> Self {
        self.previous = block.block.hash();
        self.blocks.push(block);
        self
    }

    /// Appends `count` blocks with no transactions. `salt` is what makes two
    /// chains at the same heights differ.
    #[must_use]
    pub(crate) fn run(mut self, from: u64, count: u64, salt: u8) -> Self {
        for offset in 0..count {
            self = self.block(from.saturating_add(offset), salt, Vec::new());
        }
        self
    }

    /// Appends a block carrying one transaction that spends `key_images`.
    #[must_use]
    pub(crate) fn spending(
        self,
        height: u64,
        salt: u8,
        txid: [u8; 32],
        key_images: &[[u8; 32]],
        fee: u64,
    ) -> Self {
        let prefix = TransactionPrefix {
            additional_timelock: Timelock::None,
            inputs: key_images
                .iter()
                .map(|image| Input::ToKey {
                    amount: None,
                    key_offsets: vec![1u64; usize::from(RING_LEN)],
                    key_image: CompressedPoint::from(*image),
                })
                .collect(),
            outputs: Vec::new(),
            extra: Vec::new(),
        };
        let transaction = Transaction::<Pruned>::V2 {
            prefix,
            proofs: Some(PrunedRctProofs {
                rct_type: RctType::ClsagBulletproofPlus,
                base: RctBase {
                    fee,
                    pseudo_outs: Vec::new(),
                    encrypted_amounts: Vec::new(),
                    commitments: Vec::new(),
                },
            }),
        };
        self.block(height, salt, vec![(txid, transaction)])
    }

    /// Appends a block that mined `signed` as it was broadcast, outputs and all.
    #[must_use]
    pub(crate) fn landed(mut self, height: u64, salt: u8, signed: &SignedTx) -> Self {
        let mut bytes = signed.raw.as_bytes();
        let Ok(transaction) = Transaction::<NotPruned>::read(&mut bytes) else {
            self.unbuilt = Some("the library does not read its own bytes");
            return self;
        };
        let txid = transaction.hash();
        self.block(height, salt, vec![(txid, Transaction::<Pruned>::from(transaction))])
    }

    #[must_use]
    fn block(
        mut self,
        height: u64,
        salt: u8,
        transactions: Vec<([u8; 32], Transaction<Pruned>)>,
    ) -> Self {
        let header = BlockHeader {
            hardfork_version: 16,
            hardfork_signal: 16,
            timestamp: 1_700_000_000u64.saturating_add(height.saturating_mul(120)),
            previous: self.previous,
            nonce: u32::from(salt),
        };
        let Ok(number) = usize::try_from(height) else {
            self.unbuilt = Some("a block height does not fit a usize");
            return self;
        };
        let miner = Transaction::V2 {
            prefix: TransactionPrefix {
                additional_timelock: Timelock::None,
                inputs: vec![Input::Gen(number)],
                outputs: Vec::new(),
                extra: Vec::new(),
            },
            proofs: None,
        };
        let hashes: Vec<[u8; 32]> = transactions.iter().map(|(txid, _)| *txid).collect();
        let bodies: Vec<Transaction<Pruned>> =
            transactions.into_iter().map(|(_, body)| body).collect();
        let Some(block) = Block::new(header, miner, hashes) else {
            self.unbuilt = Some("a block is not well formed");
            return self;
        };
        self.previous = block.hash();
        self.blocks.push(ScannableBlock {
            block,
            transactions: bodies,
            output_index_for_first_ringct_output: Some(0),
        });
        self
    }

    pub(crate) fn finish(self) -> Fallible<Vec<ScannableBlock>> {
        match self.unbuilt {
            Some(reason) => Err(reason.into()),
            None => Ok(self.blocks),
        }
    }
}

/// An in-memory monerod.
#[derive(Debug)]
pub(crate) struct FakeNode {
    tip: Mutex<u64>,
    blocks: Mutex<BTreeMap<u64, ScannableBlock>>,
    distribution: Mutex<Vec<u64>>,
    real_outputs: Mutex<HashMap<u64, [Point; 2]>>,
    published: Mutex<Vec<Vec<u8>>>,
    known_txids: Mutex<HashSet<[u8; 32]>>,
    refusal: Mutex<Option<Rejection>>,
    ring_endpoints: Mutex<Vec<String>>,
    broadcast_endpoints: Mutex<Vec<Option<String>>>,
    known_endpoints: Mutex<Vec<Option<String>>>,
    /// Whether a request naming an endpoint fails to connect, as a node that went down
    /// does.
    is_named_unreachable: Mutex<bool>,
    /// Positions the node answers no ring member for, as a reorganisation that took
    /// those outputs away leaves them.
    missing_members: Mutex<HashSet<u64>>,
    /// Ring requests time out until the node has served this many distributions.
    rings_time_out_until: AtomicUsize,
    fee_calls: AtomicUsize,
    distribution_calls: AtomicUsize,
    block_calls: AtomicUsize,
    blocks_served: AtomicUsize,
    known_calls: AtomicUsize,
}

impl FakeNode {
    pub(crate) fn new(tip: u64) -> Self {
        let distribution = (0..DISTRIBUTION_BLOCKS)
            .map(|index| index.saturating_add(1).saturating_mul(OUTPUTS_PER_BLOCK))
            .collect();
        Self {
            tip: Mutex::new(tip),
            blocks: Mutex::new(BTreeMap::new()),
            distribution: Mutex::new(distribution),
            real_outputs: Mutex::new(HashMap::new()),
            published: Mutex::new(Vec::new()),
            known_txids: Mutex::new(HashSet::new()),
            refusal: Mutex::new(None),
            ring_endpoints: Mutex::new(Vec::new()),
            broadcast_endpoints: Mutex::new(Vec::new()),
            known_endpoints: Mutex::new(Vec::new()),
            is_named_unreachable: Mutex::new(false),
            missing_members: Mutex::new(HashSet::new()),
            rings_time_out_until: AtomicUsize::new(0),
            fee_calls: AtomicUsize::new(0),
            distribution_calls: AtomicUsize::new(0),
            block_calls: AtomicUsize::new(0),
            blocks_served: AtomicUsize::new(0),
            known_calls: AtomicUsize::new(0),
        }
    }

    /// Replaces the chain the node serves. Heights not present are absent, which
    /// is what a request past the tip gets.
    pub(crate) fn serve(&self, blocks: Vec<ScannableBlock>) {
        let mut guard = self.blocks.lock();
        guard.clear();
        for block in blocks {
            guard.insert(u64::try_from(block.block.number()).unwrap_or(u64::MAX), block);
        }
    }

    pub(crate) fn set_tip(&self, tip: u64) {
        *self.tip.lock() = tip;
    }

    /// Registers an output the wallet really owns, so the ring the library builds
    /// contains the output actually being spent, which it checks byte for byte.
    pub(crate) fn register_real(&self, output: &WalletOutput) {
        self.real_outputs
            .lock()
            .insert(output.index_on_blockchain(), [output.key(), output.commitment().commit()]);
    }

    pub(crate) fn published(&self) -> Vec<Vec<u8>> {
        self.published.lock().clone()
    }

    /// Simulates every mempool forgetting a transaction.
    pub(crate) fn forget(&self, txid: &[u8; 32]) {
        self.known_txids.lock().remove(txid);
    }

    pub(crate) fn refuse_with(&self, refusal: Option<Rejection>) {
        *self.refusal.lock() = refusal;
    }

    pub(crate) fn ring_endpoints(&self) -> Vec<String> {
        self.ring_endpoints.lock().clone()
    }

    pub(crate) fn broadcast_endpoints(&self) -> Vec<Option<String>> {
        self.broadcast_endpoints.lock().clone()
    }

    pub(crate) fn known_endpoints(&self) -> Vec<Option<String>> {
        self.known_endpoints.lock().clone()
    }

    pub(crate) fn set_named_unreachable(&self, is_unreachable: bool) {
        *self.is_named_unreachable.lock() = is_unreachable;
    }

    pub(crate) fn drop_member(&self, position: u64) {
        self.missing_members.lock().insert(position);
    }

    /// Replaces the cumulative `RingCT` output count the node reports at each block.
    pub(crate) fn distribute(&self, outputs_through: impl Fn(u64) -> u64) {
        *self.distribution.lock() = (0..DISTRIBUTION_BLOCKS).map(outputs_through).collect();
    }

    /// Ring requests time out until the node has served `count` distributions, as at a
    /// node that dropped after serving one, until the pool answers from a node that is
    /// up.
    pub(crate) fn time_out_rings_until_distribution(&self, count: usize) {
        self.rings_time_out_until.store(count, Ordering::SeqCst);
    }

    /// The failure a request naming an endpoint meets while that node is down.
    fn refuse_named(&self, endpoint: Option<&str>) -> Result<(), MoneroError> {
        if endpoint.is_some() && *self.is_named_unreachable.lock() {
            return Err(MoneroError::Net(NetError::endpoint(
                &endpoint_label(),
                FailureKind::Connect,
                io::Error::other("connection refused"),
            )));
        }
        Ok(())
    }

    pub(crate) fn fee_calls(&self) -> usize {
        self.fee_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn distribution_calls(&self) -> usize {
        self.distribution_calls.load(Ordering::SeqCst)
    }

    /// Every block every request has been answered with, counting a block once
    /// for each time it was sent.
    pub(crate) fn blocks_served(&self) -> usize {
        self.blocks_served.load(Ordering::SeqCst)
    }

    pub(crate) fn block_calls(&self) -> usize {
        self.block_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn known_calls(&self) -> usize {
        self.known_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl MoneroNode for FakeNode {
    async fn tip_height(&self) -> Result<u64, MoneroError> {
        Ok(*self.tip.lock())
    }

    async fn blocks(&self, range: RangeInclusive<u64>) -> Result<Vec<ScannableBlock>, MoneroError> {
        self.block_calls.fetch_add(1, Ordering::SeqCst);
        let guard = self.blocks.lock();
        let blocks: Vec<ScannableBlock> =
            range.filter_map(|height| guard.get(&height).cloned()).collect();
        self.blocks_served.fetch_add(blocks.len(), Ordering::SeqCst);
        Ok(blocks)
    }

    async fn fee_ladder(&self) -> Result<FeeLadder, MoneroError> {
        self.fee_calls.fetch_add(1, Ordering::SeqCst);
        FeeLadder::new(RATES, 1)
            .ok_or(MoneroError::Internal { context: "the fake quoted a zero rate" })
    }

    async fn distribution(&self, height: u64) -> Result<Distribution, MoneroError> {
        self.distribution_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Distribution::new(self.distribution.lock().clone(), height, ENDPOINT))
    }

    async fn ring_members(
        &self,
        endpoint: &str,
        indexes: &[u64],
    ) -> Result<Vec<Option<[Point; 2]>>, MoneroError> {
        if self.distribution_calls() < self.rings_time_out_until.load(Ordering::SeqCst) {
            return Err(MoneroError::Net(NetError::Timeout {
                endpoint: endpoint_label(),
                elapsed_millis: 30_000,
            }));
        }
        self.ring_endpoints.lock().push(endpoint.to_owned());
        self.refuse_named(Some(endpoint))?;
        let real_outputs = self.real_outputs.lock();
        let missing = self.missing_members.lock();
        Ok(indexes
            .iter()
            .map(|index| {
                (!missing.contains(index)).then(|| {
                    real_outputs.get(index).copied().unwrap_or_else(|| {
                        [unowned_point(0xa1, *index), unowned_point(0xb2, *index)]
                    })
                })
            })
            .collect())
    }

    async fn broadcast(
        &self,
        endpoint: Option<&str>,
        transaction: &Transaction,
    ) -> Result<BroadcastReply, MoneroError> {
        self.broadcast_endpoints.lock().push(endpoint.map(str::to_owned));
        self.refuse_named(endpoint)?;
        let refusal = self.refusal.lock().clone();
        if let Some(rejection) = refusal {
            return Ok(BroadcastReply::Refused(rejection));
        }
        self.published.lock().push(transaction.serialize());
        self.known_txids.lock().insert(transaction.hash());
        Ok(BroadcastReply::Accepted)
    }

    async fn is_known(&self, endpoint: Option<&str>, txid: [u8; 32]) -> Result<bool, MoneroError> {
        self.known_calls.fetch_add(1, Ordering::SeqCst);
        self.known_endpoints.lock().push(endpoint.map(str::to_owned));
        self.refuse_named(endpoint)?;
        Ok(self.known_txids.lock().contains(&txid))
    }
}

/// The adapter under test, watching the fixture account's primary address, and
/// synced unless built with [`Harness::unsynced`].
#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) node: Arc<FakeNode>,
    pub(crate) adapter: MoneroAdapter,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) cursors: Arc<MemoryCursorStore>,
    pub(crate) recorded: Recorded,
    pub(crate) account: MoneroAccount,
    spend: [u8; 32],
    pub(crate) primary: String,
    pub(crate) height: u64,
    pub(crate) tip: u64,
    pub(crate) outputs: Vec<WalletOutput>,
}

/// The address id the harness watches the primary address under.
pub(crate) const PRIMARY_ID: i64 = 7;

pub(crate) fn config() -> MoneroConfig {
    MoneroConfig {
        batch_blocks: 500,
        blocks_per_pass: 5_000,
        reorg_overlap_blocks: 4,
        lookahead_major: 2,
        lookahead_minor: 2,
        checkpoint_interval: Duration::from_secs(0),
        ..MoneroConfig::default()
    }
}

/// The recorded block with empty blocks on top. A test adds its own blocks by
/// continuing the builder.
pub(crate) fn base_chain(height: u64) -> Fallible<ChainBuilder> {
    let builder = ChainBuilder::new([0; 32]).recorded(recorded_block(&recorded()?)?);
    Ok(builder.run(height.saturating_add(1), CHAIN_BLOCKS, 1))
}

/// A payment of `amount` from the primary address the harness watches to `to`.
pub(crate) fn payment(to: &str, amount: u64) -> TxRequest {
    TxRequestBuilder::new(PRIMARY_ID, to, AssetId::Native(ChainId::Monero), Amount::from(amount))
        .fee(FeeSelection::NORMAL)
        .build()
}

pub(crate) fn watched(id: i64, major: u32, minor: u32, address: &str) -> WatchedAddress {
    WatchedAddressBuilder::new(id, ChainId::Monero, address).subaddress(major, minor).build()
}

impl Harness {
    /// The recorded block followed by [`CHAIN_BLOCKS`] empty ones, synced.
    pub(crate) async fn new() -> Fallible<Self> {
        Self::with(config()).await
    }

    pub(crate) async fn with(config: MoneroConfig) -> Fallible<Self> {
        let harness = Self::unsynced(config).await?;
        harness.sync().await?;
        Ok(harness)
    }

    /// The recorded block followed by [`CHAIN_BLOCKS`] empty ones, with no pass run
    /// yet.
    pub(crate) async fn unsynced(config: MoneroConfig) -> Fallible<Self> {
        let recorded = recorded()?;
        let height = recorded_height(&recorded)?;
        let tip = height.saturating_add(CHAIN_BLOCKS);
        let account = account(&recorded)?;
        let spend = bytes32(&recorded.spend_key)?;
        let outputs = scanned(&recorded)?;

        let node = Arc::new(FakeNode::new(tip));
        node.serve(base_chain(height)?.finish()?);
        for output in &outputs {
            node.register_real(output);
        }

        let clock = Arc::new(FakeClock::default());
        let cursors = Arc::new(MemoryCursorStore::new());
        let adapter = MoneroAdapter::builder(
            Arc::<FakeNode>::clone(&node),
            account.clone(),
            Arc::<MemoryCursorStore>::clone(&cursors),
            Arc::<FakeClock>::clone(&clock),
            height,
        )
        .config(config)
        .build()
        .await?;
        let primary = account.address(0, 0)?;
        adapter.set_watch_set(&[watched(PRIMARY_ID, 0, 0, &primary)]).await?;

        Ok(Self {
            node,
            adapter,
            clock,
            cursors,
            recorded,
            account,
            spend,
            primary,
            height,
            tip,
            outputs,
        })
    }

    /// One pass, with the events it emitted.
    pub(crate) async fn sync(&self) -> Fallible<(ChainSyncProgress, Vec<WalletEvent>)> {
        let events = FakeEventSink::new();
        let deadline = self.clock.deadline_in(Duration::from_secs(30));
        let progress = self.adapter.sync(deadline, &events).await?;
        Ok((progress, events.drain()))
    }

    /// A payment of `amount` from the primary address to itself, or a sweep.
    pub(crate) fn request(&self, amount: Option<u64>) -> TxRequest {
        let builder = TxRequestBuilder::new(
            PRIMARY_ID,
            &self.primary,
            AssetId::Native(ChainId::Monero),
            Amount::from(amount.unwrap_or_default()),
        )
        .fee(FeeSelection::NORMAL);
        if amount.is_some() { builder.build() } else { builder.send_max().build() }
    }

    pub(crate) fn spend_key(&self) -> KeyMaterial {
        KeyMaterial::new(KeyKind::MoneroSpend, Zeroizing::new(self.spend.to_vec()))
    }

    /// Builds, signs and broadcasts one payment, answering with what it sent.
    pub(crate) async fn send(&self, amount: Option<u64>) -> Fallible<(UnsignedTx, SignedTx)> {
        let unsigned = self.adapter.build_tx(&self.request(amount)).await?;
        let signed = self.adapter.sign_tx(&unsigned, &[self.spend_key()])?;
        self.adapter.broadcast(&signed).await?;
        Ok((unsigned, signed))
    }

    /// The key images this wallet's own outputs derive to.
    pub(crate) fn own_key_images(&self) -> Vec<[u8; 32]> {
        self.outputs.iter().map(|output| self.account.key_image(output)).collect()
    }
}

/// A second adapter over the harness's node, clock and cursor store, watching the
/// primary address of `account`, which is what an unlock after a lock is.
pub(crate) async fn reopen(
    harness: &Harness,
    account: MoneroAccount,
    restore_height: u64,
    config: MoneroConfig,
) -> Fallible<MoneroAdapter> {
    let adapter = MoneroAdapter::builder(
        Arc::<FakeNode>::clone(&harness.node),
        account.clone(),
        Arc::<MemoryCursorStore>::clone(&harness.cursors),
        Arc::<FakeClock>::clone(&harness.clock),
        restore_height,
    )
    .config(config)
    .build()
    .await?;
    adapter.set_watch_set(&[watched(PRIMARY_ID, 0, 0, &account.address(0, 0)?)]).await?;
    Ok(adapter)
}

/// The value of one detail row, whichever list it came from.
pub(crate) fn row<'a>(details: &'a [TxDetail], label: &str) -> Option<&'a str> {
    details.iter().find(|detail| detail.label == label).map(|detail| detail.value.as_str())
}

/// Every balance announced for `address_id`, in the order the events went out.
pub(crate) fn balances_announced(events: &[WalletEvent], address_id: i64) -> Vec<Amount> {
    events
        .iter()
        .filter_map(|event| match event {
            WalletEvent::BalanceChanged(update) if update.address_id == address_id => {
                Some(update.balance)
            }
            WalletEvent::BalanceChanged(_)
            | WalletEvent::TxUpdated(_)
            | WalletEvent::TxDetected(_)
            | WalletEvent::TxConfirmed(_)
            | WalletEvent::SyncProgress(_) => None,
        })
        .collect()
}

/// What runs before each block request a [`HookedNode`] serves, given the range asked
/// for.
pub(crate) type BlocksHook =
    Box<dyn Fn(&RangeInclusive<u64>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// A [`FakeNode`] whose block requests run a hook first, so a test can slow, hold or
/// stall one.
pub(crate) struct HookedNode {
    inner: Arc<FakeNode>,
    before_blocks: BlocksHook,
}

impl fmt::Debug for HookedNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookedNode").field("inner", &self.inner).finish_non_exhaustive()
    }
}

impl HookedNode {
    pub(crate) fn new(
        inner: Arc<FakeNode>,
        before_blocks: impl Fn(&RangeInclusive<u64>) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self { inner, before_blocks: Box::new(before_blocks) }
    }
}

#[async_trait]
impl MoneroNode for HookedNode {
    async fn tip_height(&self) -> Result<u64, MoneroError> {
        self.inner.tip_height().await
    }

    async fn blocks(&self, range: RangeInclusive<u64>) -> Result<Vec<ScannableBlock>, MoneroError> {
        (self.before_blocks)(&range).await;
        self.inner.blocks(range).await
    }

    async fn fee_ladder(&self) -> Result<FeeLadder, MoneroError> {
        self.inner.fee_ladder().await
    }

    async fn distribution(&self, height: u64) -> Result<Distribution, MoneroError> {
        self.inner.distribution(height).await
    }

    async fn ring_members(
        &self,
        endpoint: &str,
        indexes: &[u64],
    ) -> Result<Vec<Option<[Point; 2]>>, MoneroError> {
        self.inner.ring_members(endpoint, indexes).await
    }

    async fn broadcast(
        &self,
        endpoint: Option<&str>,
        transaction: &Transaction,
    ) -> Result<BroadcastReply, MoneroError> {
        self.inner.broadcast(endpoint, transaction).await
    }

    async fn is_known(&self, endpoint: Option<&str>, txid: [u8; 32]) -> Result<bool, MoneroError> {
        self.inner.is_known(endpoint, txid).await
    }
}

/// An adapter for the fixture account over `node`, born at the recorded block and
/// watching the primary address.
pub(crate) async fn fixture_adapter(
    node: Arc<dyn MoneroNode>,
    cursors: Arc<dyn CursorStore>,
    clock: Arc<dyn Clock>,
    config: MoneroConfig,
) -> Fallible<MoneroAdapter> {
    let recorded = recorded()?;
    let account = account(&recorded)?;
    let height = recorded_height(&recorded)?;
    let adapter = MoneroAdapter::builder(node, account.clone(), cursors, clock, height)
        .config(config)
        .build()
        .await?;
    adapter.set_watch_set(&[watched(PRIMARY_ID, 0, 0, &account.address(0, 0)?)]).await?;
    Ok(adapter)
}

pub(crate) fn progress_event_count(events: &[WalletEvent]) -> usize {
    events.iter().filter(|event| matches!(event, WalletEvent::SyncProgress(_))).count()
}

pub(crate) fn balance_event_count(events: &[WalletEvent]) -> usize {
    events.iter().filter(|event| matches!(event, WalletEvent::BalanceChanged(_))).count()
}

/// The key images a signed transaction consumes, read back from its bytes.
pub(crate) fn images_in(signed: &SignedTx) -> io::Result<Vec<[u8; 32]>> {
    let mut bytes = signed.raw.as_bytes();
    let transaction = Transaction::<NotPruned>::read(&mut bytes)?;
    Ok(transaction
        .prefix()
        .inputs
        .iter()
        .filter_map(|input| match input {
            Input::ToKey { key_image, .. } => Some(key_image.to_bytes()),
            Input::Gen(_) => None,
        })
        .collect())
}

/// The positions on the blockchain of every ring member each input of `signed` names,
/// in input order.
pub(crate) fn rings_in(signed: &SignedTx) -> io::Result<Vec<Vec<u64>>> {
    let mut bytes = signed.raw.as_bytes();
    let transaction = Transaction::<NotPruned>::read(&mut bytes)?;
    Ok(transaction
        .prefix()
        .inputs
        .iter()
        .filter_map(|input| match input {
            Input::ToKey { key_offsets, .. } => Some(
                key_offsets
                    .iter()
                    .scan(0u64, |position, offset| {
                        *position = position.saturating_add(*offset);
                        Some(*position)
                    })
                    .collect(),
            ),
            Input::Gen(_) => None,
        })
        .collect())
}
