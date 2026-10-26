//! Every test drives the shipped engine end to end. Only the clock, the credential
//! store, and the adapters are fakes.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    AdapterBuilder, AdapterSet, Address, Amount, AssetId, BalanceUpdate, BuildRequest,
    ChainAdapter, ChainError, ChainId, ChainOutcome, ChainSyncProgress, CursorStore, Deadline,
    EventSink, FeeBump, FeeQuote, FeeRate, FeeSelection, KeyKind, KeyMaterial, PendingTracker,
    PhraseWords, RawTx, Rejection, RejectionKind, RetryPolicy, ScriptProbe, SecretText, SendAmount,
    SignedTx, SkipReason, Subaddresses, TxCancel, TxDirection, TxId, TxPayload, TxPreview,
    TxRecord, TxRequest, UnsignedTx, WalletColor, WalletEvent, WatchedAddress,
};
use jelly_engine::{
    CreatedWallet, EngineConfig, EngineDeps, PassphraseInput, WalletEngine,
    test_util::{self, BreakableClock},
};
use jelly_vault::{MasterKeyStore, test_util::MemoryKeyStore};
use parking_lot::Mutex;
use secrecy::SecretString;
use tempfile::TempDir;
use tokio::sync::{Notify, broadcast, watch};
use tokio::time::sleep;
use zeroize::Zeroizing;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// The password every wallet in this suite uses.
pub(crate) const PASSWORD: &str = "correct horse battery staple";
/// A second password, for the rotation and re-authentication tests.
pub(crate) const OTHER_PASSWORD: &str = "trombone marmalade ninety";

pub(crate) fn secret(value: &str) -> SecretString {
    SecretString::from(value)
}

/// An engine on a real temporary directory with an in-memory credential store, unless a
/// test hands it another one.
pub(crate) struct Harness<K = MemoryKeyStore> {
    pub(crate) engine: WalletEngine,
    pub(crate) clock: Arc<BreakableClock>,
    pub(crate) builder: Arc<FakeBuilder>,
    pub(crate) keystore: Arc<K>,
    /// Declared last so the engine drops before the directory is removed.
    pub(crate) scratch: TempDir,
}

impl<K> fmt::Debug for Harness<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Harness").field("directory", &self.scratch.path()).finish_non_exhaustive()
    }
}

pub(crate) async fn harness() -> Fallible<Harness> {
    harness_with(test_util::config()).await
}

pub(crate) async fn harness_with(config: EngineConfig) -> Fallible<Harness> {
    harness_in(config, tempfile::tempdir()?, Arc::default()).await
}

/// An engine whose master keys go to `keystore`, for a test that needs a credential
/// store that misbehaves.
pub(crate) async fn harness_with_keystore<K: MasterKeyStore + 'static>(
    keystore: Arc<K>,
) -> Fallible<Harness<K>> {
    harness_in(test_util::config(), tempfile::tempdir()?, keystore).await
}

/// An engine on a directory and credential store an earlier engine may have left
/// behind.
pub(crate) async fn harness_in<K: MasterKeyStore + 'static>(
    config: EngineConfig,
    scratch: TempDir,
    keystore: Arc<K>,
) -> Fallible<Harness<K>> {
    let clock = Arc::new(BreakableClock::default());
    let builder = Arc::new(FakeBuilder::default());
    let deps = EngineDeps {
        data_dir: scratch.path().to_path_buf(),
        clock: Arc::<BreakableClock>::clone(&clock),
        keystore: Arc::<K>::clone(&keystore),
        builder: Arc::<FakeBuilder>::clone(&builder),
        table: jelly_chains::chains(),
    };
    let started = WalletEngine::start(config, deps).await.map_err(|failure| failure.error)?;
    Ok(Harness { engine: started.engine, clock, builder, keystore, scratch })
}

impl<K: MasterKeyStore> Harness<K> {
    /// Creates a seed wallet with no passphrase and leaves it open.
    pub(crate) async fn seed_wallet(&self, name: &str) -> Fallible<CreatedWallet> {
        Ok(self
            .engine
            .create_seed_wallet(
                name,
                WalletColor::default(),
                secret(PASSWORD),
                PhraseWords::Twelve,
                PassphraseInput::None,
            )
            .await?)
    }

    /// Shuts the engine down and hands back its directory, which nothing holds open
    /// once this returns.
    pub(crate) async fn shut_down(self) -> Fallible<TempDir> {
        let Self { engine, scratch, .. } = self;
        engine.shutdown().await?;
        Ok(scratch)
    }
}

pub(crate) fn prompt(value: &str) -> PassphraseInput {
    PassphraseInput::Prompt { value: SecretText::new(value.to_owned()) }
}

pub(crate) fn stored(value: &str) -> PassphraseInput {
    PassphraseInput::Stored { value: SecretText::new(value.to_owned()) }
}

/// Everything a fake adapter records and every knob a test can turn.
///
/// It lives in the builder rather than the adapter, so it survives a rebuild and
/// a test can watch the adapter itself die with its session.
#[derive(Debug, Default)]
pub(crate) struct FakeState {
    syncs: AtomicU64,
    watch_sets: watch::Sender<u64>,
    broadcasts: AtomicU64,
    pub(crate) signs: AtomicU64,
    /// How many keys each signature was handed, in order.
    pub(crate) signed_key_counts: Mutex<Vec<usize>>,
    pub(crate) windows: AtomicU64,
    /// Numbers the transaction each pass reports, so every pass reports a new one.
    pass_number: AtomicU64,
    pub(crate) last_watch_set: Mutex<Vec<WatchedAddress>>,
    /// The sources the last build named, which a replacement keeps spending from.
    last_from: Mutex<Vec<String>>,
    /// Every preview a build handed out. Like a real signer that rebuilds the preview from
    /// the payload, `sign_tx` refuses any other.
    built_previews: Mutex<Vec<TxPreview>>,
    discarded: Mutex<Vec<Vec<u8>>>,
    pub(crate) should_fail_watch_set: AtomicBool,
    pub(crate) should_fail_sync: AtomicBool,
    pub(crate) should_hang_sync: AtomicBool,
    /// The address each pass reports a new balance and transaction for.
    emits_for: Mutex<Option<i64>>,
    pub(crate) tip: AtomicU64,
    pub(crate) bump_floor_sat_per_kwu: AtomicU64,
    /// Set to make `broadcast` refuse with this rejection.
    refusal: Mutex<Option<Rejection>>,
    /// Set to make every preview claim this chain instead of the adapter's own,
    /// which is what a defective adapter looks like from the engine's side.
    previewed_chain: Mutex<Option<ChainId>>,
    /// An address row the next `broadcast` deletes through this engine, so the record
    /// that follows a successful broadcast cannot be written. The broadcast takes it,
    /// because the engine holds this state through its builder and a handle left here
    /// would never be dropped.
    delete_on_broadcast: Mutex<Option<(WalletEngine, i64)>>,
    /// Set to make every `sign_tx` move this clock forward, the way a slow signer
    /// spends time.
    sign_delay: Mutex<Option<(Arc<BreakableClock>, Duration)>>,
    /// Set to make a `broadcast` the network accepts break this clock, so every write
    /// after the point of no return fails.
    clock_broken_on_broadcast: Mutex<Option<Arc<BreakableClock>>>,
    /// The addresses the script probe reports in use.
    pub(crate) used_addresses: Mutex<Vec<String>>,
    /// Every list of addresses the script probe was asked about, in order.
    pub(crate) probes: Mutex<Vec<Vec<String>>>,
    pub(crate) should_fail_probe: AtomicBool,
}

impl FakeState {
    pub(crate) fn emit_for(&self, address_id: i64) {
        *self.emits_for.lock() = Some(address_id);
    }

    pub(crate) fn stop_emitting(&self) {
        *self.emits_for.lock() = None;
    }

    pub(crate) fn break_record_after_broadcast(&self, engine: WalletEngine, address_id: i64) {
        *self.delete_on_broadcast.lock() = Some((engine, address_id));
    }

    pub(crate) fn sign_slowly(&self, clock: Arc<BreakableClock>, by: Duration) {
        *self.sign_delay.lock() = Some((clock, by));
    }

    pub(crate) fn break_clock_on_broadcast(&self, clock: Arc<BreakableClock>) {
        *self.clock_broken_on_broadcast.lock() = Some(clock);
    }

    pub(crate) fn refuse(&self, kind: RejectionKind, retry: RetryPolicy) {
        *self.refusal.lock() = Some(Rejection::new(kind, retry, "the fake network refused"));
    }

    pub(crate) fn preview_chain(&self, chain: ChainId) {
        *self.previewed_chain.lock() = Some(chain);
    }

    pub(crate) fn sync_count(&self) -> u64 {
        self.syncs.load(Ordering::SeqCst)
    }

    /// How many watch sets the adapter has been handed, refused ones included.
    pub(crate) fn watch_set_count(&self) -> u64 {
        *self.watch_sets.borrow()
    }

    /// Returns once the adapter has been handed `count` watch sets.
    pub(crate) async fn watch_sets_reach(&self, count: u64) {
        let mut handed = self.watch_sets.subscribe();
        // The state owns the sender, so the channel cannot close during the wait.
        let _ = handed.wait_for(|&so_far| so_far >= count).await;
    }

    pub(crate) fn broadcast_count(&self) -> u64 {
        self.broadcasts.load(Ordering::SeqCst)
    }

    pub(crate) fn discards(&self) -> usize {
        self.discarded.lock().len()
    }
}

/// A chain adapter that answers from memory and records what it was asked.
pub(crate) struct FakeAdapter {
    chain: ChainId,
    state: Arc<FakeState>,
    /// Stands in for the key material a real adapter holds until it is dropped.
    _key: KeyMaterial,
}

impl fmt::Debug for FakeAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeAdapter").field("chain", &self.chain).finish_non_exhaustive()
    }
}

impl FakeAdapter {
    fn new(chain: ChainId, state: Arc<FakeState>) -> Self {
        Self {
            chain,
            state,
            _key: KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(vec![7u8; 32])),
        }
    }

    const fn asset(&self) -> AssetId {
        AssetId::Native(self.chain)
    }

    fn preview(&self, to: &str, from: Vec<String>, amount: Amount) -> TxPreview {
        let preview = TxPreview {
            chain: self.state.previewed_chain.lock().unwrap_or(self.chain),
            asset: self.asset(),
            from,
            to: to.to_owned(),
            destination_tag: None,
            amount,
            fee: Amount::from_base_units(1_000),
            details: Vec::new(),
            warnings: Vec::new(),
        };
        self.state.built_previews.lock().push(preview.clone());
        preview
    }

    fn watched_address(&self, address_id: i64) -> String {
        self.state
            .last_watch_set
            .lock()
            .iter()
            .find(|watched| watched.id == address_id)
            .map_or_else(String::new, |watched| watched.address.value().to_owned())
    }
}

#[async_trait]
impl ChainAdapter for FakeAdapter {
    fn chain(&self) -> ChainId {
        self.chain
    }

    async fn set_watch_set(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        self.state.watch_sets.send_modify(|handed| *handed = handed.saturating_add(1));
        if self.state.should_fail_watch_set.load(Ordering::SeqCst) {
            return Err(ChainError::rpc("the fake watch set was refused"));
        }
        *self.state.last_watch_set.lock() = addresses.to_vec();
        Ok(())
    }

    async fn sync(
        &self,
        _deadline: Deadline,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, ChainError> {
        if self.state.should_hang_sync.load(Ordering::SeqCst) {
            sleep(Duration::from_secs(3600)).await;
        }
        if self.state.should_fail_sync.load(Ordering::SeqCst) {
            return Err(ChainError::rpc("the fake endpoint refused"));
        }
        let pass_number = self.state.pass_number.fetch_add(1, Ordering::SeqCst);

        let emits_for = *self.state.emits_for.lock();
        let tip = self.state.tip.load(Ordering::SeqCst);
        if let Some(address_id) = emits_for {
            events
                .emit(WalletEvent::BalanceChanged(BalanceUpdate {
                    address_id,
                    asset: self.asset(),
                    balance: Amount::from_base_units(u128::from(pass_number).saturating_add(1)),
                }))
                .await;
            let record = TxRecord::builder(
                self.asset(),
                address_id,
                TxId::new(self.chain, format!("txid-{pass_number}")),
                TxDirection::Incoming,
                Amount::from_base_units(500),
            )
            .confirmed_at(tip, Some(1_700_000_000), tip)
            .build();
            events.emit(WalletEvent::TxDetected(record)).await;
        }
        self.state.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(ChainSyncProgress::new(self.chain)
            .with_heights(Some(tip), Some(tip))
            .with_progress(1000))
    }

    async fn quote_fee(&self, _request: &TxRequest) -> Result<FeeQuote, ChainError> {
        Ok(FeeQuote {
            fee: Amount::from_base_units(1_000),
            custom: None,
            max_amount: None,
            levels: Vec::new(),
        })
    }

    async fn build_tx(&self, request: &TxRequest) -> Result<UnsignedTx, ChainError> {
        let amount = match request.amount {
            SendAmount::Exact { value } => value,
            SendAmount::Max => Amount::from_base_units(10_000),
        };
        let from: Vec<String> =
            request.from_address_ids.iter().map(|id| self.watched_address(*id)).collect();
        self.state.last_from.lock().clone_from(&from);
        Ok(UnsignedTx {
            preview: self.preview(&request.to, from, amount),
            payload: TxPayload::from_bytes(vec![1, 2, 3]),
        })
    }

    fn sign_tx(&self, tx: &UnsignedTx, keys: &[KeyMaterial]) -> Result<SignedTx, ChainError> {
        if keys.len() != tx.preview.from.len() || keys.iter().any(KeyMaterial::is_empty) {
            return Err(ChainError::signing("the signer was not handed a key per source"));
        }
        if !self.state.built_previews.lock().contains(&tx.preview) {
            return Err(ChainError::signing("the preview is not one this adapter built"));
        }
        self.state.signed_key_counts.lock().push(keys.len());
        if let Some((clock, by)) = self.state.sign_delay.lock().as_ref() {
            clock.advance(*by);
        }
        let count = self.state.signs.fetch_add(1, Ordering::SeqCst);
        Ok(SignedTx {
            raw: RawTx::from_bytes(vec![9, 9, 9]),
            txid: TxId::new(tx.chain(), format!("signed-{count}")),
        })
    }

    async fn broadcast(&self, tx: &SignedTx) -> Result<TxId, ChainError> {
        self.state.broadcasts.fetch_add(1, Ordering::SeqCst);
        let refusal = self.state.refusal.lock().clone();
        if let Some(refusal) = refusal {
            return Err(ChainError::Rejected(refusal));
        }
        let deleting = self.state.delete_on_broadcast.lock().take();
        if let Some((engine, address_id)) = deleting {
            engine
                .delete_address(address_id)
                .await
                .map_err(|_| ChainError::rpc("the fixture could not remove the row"))?;
        }
        if let Some(clock) = self.state.clock_broken_on_broadcast.lock().as_ref() {
            clock.set_broken(true);
        }
        Ok(tx.txid.clone())
    }

    fn fee_bump(&self) -> Option<&dyn FeeBump> {
        Some(self)
    }

    fn tx_cancel(&self) -> Option<&dyn TxCancel> {
        Some(self)
    }

    fn pending_tracker(&self) -> Option<&dyn PendingTracker> {
        Some(self)
    }

    fn subaddresses(&self) -> Option<&dyn Subaddresses> {
        Some(self)
    }

    fn script_probe(&self) -> Option<&dyn ScriptProbe> {
        Some(self)
    }
}

#[async_trait]
impl ScriptProbe for FakeAdapter {
    async fn used(&self, addresses: &[Address]) -> Result<Vec<Address>, ChainError> {
        self.state
            .probes
            .lock()
            .push(addresses.iter().map(|address| address.value().to_owned()).collect());
        if self.state.should_fail_probe.load(Ordering::SeqCst) {
            return Err(ChainError::rpc("the fake probe was refused"));
        }
        let used = self.state.used_addresses.lock();
        Ok(addresses
            .iter()
            .filter(|address| used.iter().any(|known| known == address.value()))
            .cloned()
            .collect())
    }
}

/// A cancel pays nothing back to the address the last build spent from, at the
/// fee every quote names.
#[async_trait]
impl TxCancel for FakeAdapter {
    async fn quote_cancel(
        &self,
        _txid: &TxId,
        _fee: &FeeSelection,
    ) -> Result<FeeQuote, ChainError> {
        Ok(FeeQuote {
            fee: Amount::from_base_units(1_000),
            custom: None,
            max_amount: None,
            levels: Vec::new(),
        })
    }

    async fn build_cancel(
        &self,
        _txid: &TxId,
        _fee: &FeeSelection,
    ) -> Result<UnsignedTx, ChainError> {
        let from = self.state.last_from.lock().clone();
        let to = from.first().cloned().unwrap_or_default();
        Ok(UnsignedTx {
            preview: self.preview(&to, from, Amount::ZERO),
            payload: TxPayload::from_bytes(vec![7, 8, 9]),
        })
    }
}

#[async_trait]
impl FeeBump for FakeAdapter {
    /// A level is priced at the floor a test set, with the custom fee that buys it.
    async fn quote_bump(&self, _txid: &TxId, _fee: &FeeSelection) -> Result<FeeQuote, ChainError> {
        let floor =
            FeeRate::from_sat_per_kwu(self.state.bump_floor_sat_per_kwu.load(Ordering::SeqCst));
        Ok(FeeQuote {
            fee: Amount::from(floor.sat_per_kwu()),
            custom: Some(FeeSelection::Rate { rate: floor.format_sat_per_vb() }),
            max_amount: None,
            levels: Vec::new(),
        })
    }

    /// Refuses anything below the floor a test set, which is what a real adapter
    /// does and what the engine relies on rather than checking itself.
    async fn build_bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<UnsignedTx, ChainError> {
        let floor =
            FeeRate::from_sat_per_kwu(self.state.bump_floor_sat_per_kwu.load(Ordering::SeqCst));
        let asked = match *fee {
            FeeSelection::Level { .. } => floor,
            FeeSelection::Rate { ref rate } => {
                FeeRate::parse_sat_per_vb(rate).map_err(ChainError::invalid_address)?
            }
            FeeSelection::Evm { .. } | FeeSelection::Solana { .. } | FeeSelection::Total { .. } => {
                return Err(ChainError::unsupported("that is not this chain's fee shape"));
            }
        };
        if asked < floor {
            return Err(ChainError::Rejected(Rejection::new(
                RejectionKind::BelowIncrementalRelayFee,
                RetryPolicy::DidNotLand,
                "the replacement has to pay more",
            )));
        }
        let from = self.state.last_from.lock().clone();
        Ok(UnsignedTx {
            preview: self.preview(
                &format!("replacement-of-{}", txid.value()),
                from,
                Amount::from_base_units(10_000),
            ),
            payload: TxPayload::from_bytes(vec![4, 5, 6]),
        })
    }
}

#[async_trait]
impl PendingTracker for FakeAdapter {
    async fn discard(&self, tx: &UnsignedTx) -> Result<(), ChainError> {
        self.state.discarded.lock().push(tx.payload.as_bytes().to_vec());
        Ok(())
    }
}

#[async_trait]
impl Subaddresses for FakeAdapter {
    async fn register_window(&self, _major: u32, _minor: u32) -> Result<(), ChainError> {
        self.state.windows.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Builds a fresh [`FakeAdapter`] per rebuild over shared state, and keeps only a
/// weak handle, so a test can watch the adapter die with its session.
#[derive(Debug, Default)]
pub(crate) struct FakeBuilder {
    chains: Mutex<Vec<ChainId>>,
    skips: Mutex<Vec<(ChainId, SkipReason)>>,
    states: Mutex<BTreeMap<ChainId, Arc<FakeState>>>,
    built: Mutex<BTreeMap<ChainId, Weak<FakeAdapter>>>,
    /// Every finished build's request, in order.
    requests: watch::Sender<Vec<BuildRequest>>,
    /// Set for one build, so a test can drive what happens while a generation
    /// is still being assembled.
    is_holding: AtomicBool,
    started: Notify,
    release: Notify,
}

impl FakeBuilder {
    /// The chains this builder can build, when the engine asks for them.
    pub(crate) fn install(&self, chains: &[ChainId]) {
        *self.chains.lock() = chains.to_vec();
    }

    /// Makes the next build wait for [`FakeBuilder::release`] before it builds anything.
    pub(crate) fn hold_next_build(&self) {
        self.is_holding.store(true, Ordering::SeqCst);
    }

    /// Returns once the held build has reached that point.
    pub(crate) async fn build_started(&self) {
        self.started.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }

    /// Makes the builder answer `reason` for `chain` instead of an adapter.
    pub(crate) fn skip(&self, chain: ChainId, reason: SkipReason) {
        self.skips.lock().push((chain, reason));
    }

    pub(crate) fn state(&self, chain: ChainId) -> Arc<FakeState> {
        Arc::clone(self.states.lock().entry(chain).or_default())
    }

    pub(crate) fn weak(&self, chain: ChainId) -> Weak<FakeAdapter> {
        self.built.lock().get(&chain).cloned().unwrap_or_default()
    }

    pub(crate) fn builds(&self) -> usize {
        self.requests.borrow().len()
    }

    /// Returns once `count` builds have finished.
    pub(crate) async fn builds_reach(&self, count: usize) {
        let mut finished = self.requests.subscribe();
        // The builder owns the sender, so the channel cannot close during the wait.
        let _ = finished.wait_for(|requests| requests.len() >= count).await;
    }

    pub(crate) fn last_request(&self) -> Option<BuildRequest> {
        self.requests.borrow().last().cloned()
    }

    /// The cursor port the engine handed to the last build.
    pub(crate) fn cursors(&self) -> Option<Arc<dyn CursorStore>> {
        self.requests.borrow().last().map(|request| Arc::clone(&request.cursors))
    }
}

#[async_trait]
impl AdapterBuilder for FakeBuilder {
    async fn build(&self, request: &BuildRequest) -> AdapterSet {
        if self.is_holding.swap(false, Ordering::SeqCst) {
            self.started.notify_one();
            self.release.notified().await;
        }
        let installed = self.chains.lock().clone();
        let skips = self.skips.lock().clone();
        let mut outcomes: Vec<(ChainId, ChainOutcome)> = Vec::new();
        for (chain, reason) in skips {
            if request.chains.contains(&chain) {
                outcomes.push((chain, ChainOutcome::Skipped(reason)));
            }
        }
        for chain in installed {
            if !request.chains.contains(&chain) || outcomes.iter().any(|(found, _)| *found == chain)
            {
                continue;
            }
            let adapter = Arc::new(FakeAdapter::new(chain, self.state(chain)));
            self.built.lock().insert(chain, Arc::downgrade(&adapter));
            outcomes.push((chain, ChainOutcome::Built(adapter)));
        }
        self.requests.send_modify(|requests| requests.push(request.clone()));
        outcomes.into_iter().collect()
    }
}

/// Waits for the pump to rebroadcast one detected transaction, which is the
/// signal that the batch behind it has been written.
pub(crate) async fn next_detected(
    events: &mut broadcast::Receiver<WalletEvent>,
) -> Fallible<String> {
    loop {
        if let WalletEvent::TxDetected(record) = events.recv().await? {
            return Ok(record.txid.value().to_owned());
        }
    }
}

/// The txid of the next transaction announced, whether new to the wallet or not.
pub(crate) async fn next_announced(
    events: &mut broadcast::Receiver<WalletEvent>,
) -> Fallible<String> {
    loop {
        if let WalletEvent::TxDetected(record) | WalletEvent::TxUpdated(record) =
            events.recv().await?
        {
            return Ok(record.txid.value().to_owned());
        }
    }
}

/// Everything already in the broadcast buffer. Callers wait for what they expect first.
pub(crate) fn drain(events: &mut broadcast::Receiver<WalletEvent>) -> Vec<WalletEvent> {
    let mut drained = Vec::new();
    while let Ok(event) = events.try_recv() {
        drained.push(event);
    }
    drained
}

/// Every transaction announced so far, draining whatever else is queued.
///
/// A sync loop publishes each pass's terminal progress straight to this channel, so an
/// empty channel is never a safe assertion. Tests assert that nothing was detected
/// instead.
pub(crate) fn detected_so_far(events: &mut broadcast::Receiver<WalletEvent>) -> Vec<String> {
    drain(events)
        .into_iter()
        .filter_map(|event| {
            if let WalletEvent::TxDetected(record) = event {
                Some(record.txid.value().to_owned())
            } else {
                None
            }
        })
        .collect()
}

/// Returns once everything queued into the event pump before the call has been through
/// it, written or refused, vault cursors included.
///
/// A window publishes its progress whether or not its write landed, and only after its
/// vault cursors. The pump gives up on a refused window only after that, so a second
/// marker, which cannot share the first one's window, proves that window is done.
pub(crate) async fn settled(engine: &WalletEngine) -> Fallible<()> {
    let sink = test_util::event_sink(engine);
    let mut events = engine.subscribe();
    for permille in [1, 2] {
        let marker = ChainSyncProgress::new(MARKER_CHAIN).with_progress(permille);
        sink.emit(WalletEvent::SyncProgress(marker)).await;
        while events.recv().await? != WalletEvent::SyncProgress(marker) {}
    }
    Ok(())
}

/// The chain the markers of [`settled`] report progress on, which no test syncs.
const MARKER_CHAIN: ChainId = ChainId::Avalanche;

/// An incoming bitcoin transaction of one base unit at `address_id`.
pub(crate) fn record(address_id: i64, txid: &str) -> TxRecord {
    TxRecord::builder(
        AssetId::Native(ChainId::Bitcoin),
        address_id,
        TxId::new(ChainId::Bitcoin, txid),
        TxDirection::Incoming,
        Amount::from_base_units(1),
    )
    .seen_at(1_700_000_000)
    .build()
}
