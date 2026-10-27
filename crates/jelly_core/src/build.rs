use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::adapter::ChainAdapter;
use crate::chain::ChainId;
use crate::cursor::CursorStore;
use crate::error::ChainError;
use crate::proxy::ProxySetting;
use crate::secret::{KeyMaterial, SecretText};
use crate::transport::Transport;
use crate::wire_enum;

wire_enum! {
    /// A third-party service a stored API key unlocks. An adapter asks for the key of the
    /// service it talks to, so which chains share a key is no concern of the engine's.
    pub enum KeyService {
        Etherscan => "etherscan",
        TronGrid => "trongrid",
    }
}

/// Session data and settings for one adapter rebuild, read once so every chain in a
/// generation sees the same settings.
///
/// Cursors retain the wallet ID so a retired adapter cannot write into the next session's
/// vault. The API keys are as stored, where a cleared one can be an empty string, and
/// [`BuildRequest::with_api_key`] reads through that. `Debug` reports only which services have
/// a key, so a log never echoes one.
#[derive(Clone)]
pub struct BuildRequest {
    pub chains: Vec<ChainId>,
    pub cursors: Arc<dyn CursorStore>,
    pub proxy: Option<ProxySetting>,
    pub api_keys: BTreeMap<KeyService, SecretText>,
    /// The height a chain's scan may start from, for a chain whose wallet records a
    /// birthday. Zero is the safe default, because a height above the true birthday
    /// silently misses funds while one below only costs scanning time.
    pub restore_heights: BTreeMap<ChainId, u64>,
    /// The account key of each chain whose spec needs one to scan, when the open wallet
    /// holds it.
    pub account_keys: BTreeMap<ChainId, Arc<KeyMaterial>>,
}

impl BuildRequest {
    /// A cleared field is stored as an empty string. A blank key must mean no service,
    /// because a history client rejects one and would fail the whole adapter.
    pub fn with_api_key<R>(&self, service: KeyService, read: impl FnOnce(Option<&str>) -> R) -> R {
        match self.api_keys.get(&service) {
            Some(key) => key.with_str(|key| read(Some(key.trim()).filter(|key| !key.is_empty()))),
            None => read(None),
        }
    }

    pub fn restore_height(&self, chain: ChainId) -> u64 {
        self.restore_heights.get(&chain).copied().unwrap_or_default()
    }

    pub fn account_key(&self, chain: ChainId) -> Option<&KeyMaterial> {
        self.account_keys.get(&chain).map(Arc::as_ref)
    }
}

impl fmt::Debug for BuildRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuildRequest")
            .field("chains", &self.chains)
            .field("cursors", &self.cursors)
            .field("proxy", &self.proxy)
            .field("api_keys_for", &self.api_keys.keys())
            .field("restore_heights", &self.restore_heights)
            .field("account_keys_for", &self.account_keys.keys())
            .finish()
    }
}

/// Why one chain has no adapter after a build.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{} has no {transport} endpoint configured", chain.display_name())]
    NoEndpoints { chain: ChainId, transport: Transport },
    /// The chain cannot be scanned from public data, so an adapter for it exists only
    /// while the session holds the account key.
    #[error("{} cannot sync without its account key", chain.display_name())]
    NoKeys { chain: ChainId },
    #[error(transparent)]
    Failed(#[from] ChainError),
}

impl BuildError {
    /// An empty rotation and a missing key are answers rather than failures, and the
    /// registry reports them differently from a build that broke. Everything else is a
    /// failed build that the next rebuild retries.
    pub const fn skip_reason(&self) -> SkipReason {
        match *self {
            Self::NoEndpoints { .. } => SkipReason::NoEndpoints,
            Self::NoKeys { .. } => SkipReason::NoKeys,
            Self::Failed(_) => SkipReason::BuildFailed,
        }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Why a chain has no adapter after a rebuild. The engine already knows which chains
    /// the wallet switched off, so a builder never reports that.
    ///
    /// The reasons are separate because each is a different problem. An empty rotation is
    /// configuration, a missing key is a permanent property of the wallet, and anything
    /// else is a failure the next rebuild retries.
    pub enum SkipReason {
        /// The endpoint registry offered nothing to talk to.
        NoEndpoints => "no_endpoints",
        /// This wallet holds no key the chain can be scanned or spent with, and
        /// no rebuild will change that.
        NoKeys => "no_keys",
        /// Endpoints exist but the adapter could not be constructed.
        BuildFailed => "build_failed",
    }
}

/// What a builder decided about one chain. A chain either has an adapter or has
/// a reason, so a chain that is silently missing is a builder defect the engine
/// can name.
#[derive(Debug, Clone)]
pub enum ChainOutcome {
    Built(Arc<dyn ChainAdapter>),
    Skipped(SkipReason),
}

/// The adapters for one unlocked wallet.
///
/// Published as one immutable generation, so the sync loops and the command
/// layer always read the same set and never a half-rebuilt one. Build one by
/// collecting outcomes, because there is deliberately no mutating insert.
#[derive(Debug, Default)]
pub struct AdapterSet {
    outcomes: BTreeMap<ChainId, ChainOutcome>,
}

impl AdapterSet {
    pub fn adapter(&self, chain: ChainId) -> Option<&Arc<dyn ChainAdapter>> {
        match self.outcomes.get(&chain) {
            Some(ChainOutcome::Built(adapter)) => Some(adapter),
            Some(ChainOutcome::Skipped(_)) | None => None,
        }
    }

    pub fn outcome(&self, chain: ChainId) -> Option<&ChainOutcome> {
        self.outcomes.get(&chain)
    }

    /// The chains that have an adapter, in chain order.
    pub fn built(&self) -> Vec<ChainId> {
        self.adapters().map(|(chain, _)| chain).collect()
    }

    /// Every built adapter with its chain, in chain order.
    pub fn adapters(&self) -> impl Iterator<Item = (ChainId, &Arc<dyn ChainAdapter>)> {
        self.outcomes.iter().filter_map(|(&chain, outcome)| match *outcome {
            ChainOutcome::Built(ref adapter) => Some((chain, adapter)),
            ChainOutcome::Skipped(_) => None,
        })
    }
}

impl FromIterator<(ChainId, ChainOutcome)> for AdapterSet {
    fn from_iter<I: IntoIterator<Item = (ChainId, ChainOutcome)>>(iter: I) -> Self {
        Self { outcomes: iter.into_iter().collect() }
    }
}

/// How the engine obtains adapters. Implemented by the chain registry, because building
/// one needs endpoints and every chain crate.
#[async_trait]
pub trait AdapterBuilder: Send + Sync + fmt::Debug {
    async fn build(&self, request: &BuildRequest) -> AdapterSet;
}

#[cfg(test)]
mod tests {
    use std::iter::once;

    use super::*;
    use crate::test_util::build_request;

    fn request_with_keys(api_keys: BTreeMap<KeyService, SecretText>) -> BuildRequest {
        BuildRequest { api_keys, ..build_request(&[ChainId::Ethereum]) }
    }

    #[test]
    fn a_collected_set_keeps_each_chains_reason() {
        let set: AdapterSet = [
            (ChainId::Tron, ChainOutcome::Skipped(SkipReason::NoEndpoints)),
            (ChainId::Bitcoin, ChainOutcome::Skipped(SkipReason::BuildFailed)),
        ]
        .into_iter()
        .collect();
        assert!(set.built().is_empty());
        assert!(matches!(
            set.outcome(ChainId::Bitcoin),
            Some(ChainOutcome::Skipped(SkipReason::BuildFailed))
        ));
        assert!(matches!(
            set.outcome(ChainId::Tron),
            Some(ChainOutcome::Skipped(SkipReason::NoEndpoints))
        ));
        assert!(set.adapter(ChainId::Bitcoin).is_none());
    }

    #[test]
    fn a_request_prints_which_services_have_a_key_but_never_the_key() {
        let request = request_with_keys(
            once((KeyService::Etherscan, SecretText::new("etherscan-secret".to_owned()))).collect(),
        );
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("etherscan-secret"), "{rendered}");
        assert!(rendered.contains("api_keys_for: [Etherscan]"), "{rendered}");
        request.with_api_key(KeyService::Etherscan, |key| {
            assert_eq!(key, Some("etherscan-secret"));
        });
        request.with_api_key(KeyService::TronGrid, |key| assert_eq!(key, None));
    }

    #[test]
    fn a_blank_api_key_is_no_key_and_a_key_is_trimmed() {
        let request = request_with_keys(
            [
                (KeyService::TronGrid, SecretText::new("   ".to_owned())),
                (KeyService::Etherscan, SecretText::new("  abc  ".to_owned())),
            ]
            .into_iter()
            .collect(),
        );
        request.with_api_key(KeyService::TronGrid, |key| assert_eq!(key, None));
        request.with_api_key(KeyService::Etherscan, |key| assert_eq!(key, Some("abc")));
    }

    #[test]
    fn a_chain_without_a_recorded_birthday_scans_from_height_zero() {
        let request = request_with_keys(BTreeMap::new());
        assert_eq!(request.restore_height(ChainId::Monero), 0);
    }
}
