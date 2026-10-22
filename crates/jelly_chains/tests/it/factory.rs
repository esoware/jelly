use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use jelly_chains::AdapterFactory;
use jelly_core::{
    AdapterBuilder as _, AdapterSet, BuildRequest, ChainAdapter, ChainId, ChainOutcome,
    EndpointOverride, KeyKind, KeyMaterial, KeyService, ProxySetting, SecretText, SkipReason,
    Transport,
    test_util::{FakeEventSink, MemoryCursorStore, build_request, key_material},
};
use jelly_net::{NetError, Network};
use tokio::time::timeout;

use crate::common::{Fallible, direct_network, serve};

const SPEND_KEY_FILL: u8 = 0x11;
const RESTORE_HEIGHT: u64 = 3_000_000;

struct Harness {
    network: Arc<Network>,
    factory: AdapterFactory,
    account_key: Arc<KeyMaterial>,
    cursors: Arc<MemoryCursorStore>,
}

impl Harness {
    fn new() -> Result<Self, NetError> {
        let network = direct_network()?;
        let factory = AdapterFactory::new();
        factory.attach(Arc::clone(&network));
        Ok(Self {
            network,
            factory,
            account_key: Arc::new(key_material(KeyKind::MoneroSpend, SPEND_KEY_FILL)),
            cursors: Arc::new(MemoryCursorStore::new()),
        })
    }

    /// A request from a wallet that holds the Monero account key.
    fn request(&self, chains: &[ChainId]) -> BuildRequest {
        BuildRequest {
            account_keys: BTreeMap::from([(ChainId::Monero, Arc::clone(&self.account_key))]),
            ..self.keyless(chains)
        }
    }

    fn keyless(&self, chains: &[ChainId]) -> BuildRequest {
        BuildRequest {
            cursors: Arc::<MemoryCursorStore>::clone(&self.cursors),
            ..build_request(chains)
        }
    }

    async fn build(&self, chains: &[ChainId]) -> AdapterSet {
        self.factory.build(&self.request(chains)).await
    }

    /// Switches off every curated endpoint of `chains` that `should_disable` selects.
    /// Loading replaces every chain's overrides, so all of them go in one load.
    fn disable(
        &self,
        chains: &[ChainId],
        should_disable: impl Fn(Transport) -> bool,
    ) -> Result<(), NetError> {
        let registry = self.network.registry();
        let mut rows = Vec::new();
        for &chain in chains {
            for entry in &registry.defaults(chain)?.endpoints {
                if should_disable(entry.transport) {
                    let row = EndpointOverride {
                        is_enabled: false,
                        ..EndpointOverride::new(entry.url.clone())
                    };
                    rows.push((chain, row));
                }
            }
        }
        registry.load_overrides(rows);
        Ok(())
    }
}

/// The API keys a wallet's settings hand the build.
fn api_keys(etherscan: &str, trongrid: &str) -> BTreeMap<KeyService, SecretText> {
    BTreeMap::from([
        (KeyService::Etherscan, SecretText::new(etherscan.to_owned())),
        (KeyService::TronGrid, SecretText::new(trongrid.to_owned())),
    ])
}

/// The optional services an adapter was built with are observable only through
/// its `Debug`.
fn rendered(set: &AdapterSet, chain: ChainId) -> Fallible<String> {
    let adapter = adapter(set, chain)?;
    Ok(format!("{adapter:?}"))
}

fn adapter(set: &AdapterSet, chain: ChainId) -> Fallible<Arc<dyn ChainAdapter>> {
    set.adapter(chain).cloned().ok_or_else(|| format!("{chain} has no adapter").into())
}

/// Every chain the set has a reason for instead of an adapter, in chain order.
fn skipped(set: &AdapterSet) -> Vec<(ChainId, SkipReason)> {
    ChainId::ALL
        .into_iter()
        .filter_map(|chain| match *set.outcome(chain)? {
            ChainOutcome::Skipped(reason) => Some((chain, reason)),
            ChainOutcome::Built(_) => None,
        })
        .collect()
}

fn every_chain_skipped_for(reason: SkipReason) -> Vec<(ChainId, SkipReason)> {
    ChainId::ALL.into_iter().map(|chain| (chain, reason)).collect()
}

#[tokio::test]
async fn every_chain_builds_from_the_curated_defaults() {
    let harness = Harness::new().unwrap();
    let set = harness.build(&ChainId::ALL).await;

    assert_eq!(set.built(), ChainId::ALL.to_vec());
    assert!(skipped(&set).is_empty(), "{:?}", skipped(&set));
    for chain in ChainId::ALL {
        assert_eq!(adapter(&set, chain).unwrap().chain(), chain);
    }
}

#[tokio::test]
async fn only_the_chains_that_were_asked_for_appear() {
    let harness = Harness::new().unwrap();
    let asked_chains = [ChainId::Bitcoin, ChainId::Solana];
    let set = harness.build(&asked_chains).await;

    assert_eq!(set.built(), asked_chains.to_vec());
    for chain in ChainId::ALL {
        assert_eq!(set.outcome(chain).is_some(), asked_chains.contains(&chain), "{chain}");
    }
}

#[tokio::test]
async fn a_factory_with_nothing_attached_skips_every_chain_as_a_failed_build() {
    let set = AdapterFactory::new().build(&Harness::new().unwrap().keyless(&ChainId::ALL)).await;
    assert!(set.built().is_empty());
    assert_eq!(skipped(&set), every_chain_skipped_for(SkipReason::BuildFailed));
}

#[tokio::test]
async fn monero_is_the_only_chain_that_cannot_be_built_without_account_keys() {
    let harness = Harness::new().unwrap();
    let set = harness.factory.build(&harness.keyless(&ChainId::ALL)).await;

    assert_eq!(skipped(&set), vec![(ChainId::Monero, SkipReason::NoKeys)]);
    assert_eq!(set.built().len(), ChainId::COUNT - 1);
}

#[tokio::test]
async fn a_chain_with_every_endpoint_disabled_is_skipped_for_having_no_endpoints() {
    let harness = Harness::new().unwrap();
    harness.disable(&ChainId::ALL, |_| true).unwrap();
    let set = harness.build(&ChainId::ALL).await;

    assert!(set.built().is_empty());
    assert_eq!(skipped(&set), every_chain_skipped_for(SkipReason::NoEndpoints));
}

#[tokio::test]
async fn a_chain_keeps_its_adapter_when_only_its_optional_service_is_gone() {
    let harness = Harness::new().unwrap();
    let chains = [ChainId::Bitcoin, ChainId::Litecoin, ChainId::Ethereum, ChainId::Bsc];
    harness
        .disable(&chains, |transport| {
            matches!(transport, Transport::FeeApi | Transport::HistoryApi)
        })
        .unwrap();
    let request =
        BuildRequest { api_keys: api_keys("test-key", ""), ..harness.request(&ChainId::ALL) };

    let set = harness.factory.build(&request).await;
    assert_eq!(set.built(), ChainId::ALL.to_vec());
    // With no history endpoint the key cannot be used, so the pass falls back to
    // walking logs.
    assert!(rendered(&set, ChainId::Ethereum).unwrap().contains("indexed_history: false"));
}

#[tokio::test]
async fn an_etherscan_key_turns_on_indexed_evm_history_and_no_adapter_prints_a_key() {
    let harness = Harness::new().unwrap();

    let unkeyed = harness.build(&ChainId::ALL).await;
    for chain in [ChainId::Polygon, ChainId::Bsc] {
        assert!(rendered(&unkeyed, chain).unwrap().contains("indexed_history: false"), "{chain}");
    }

    let request = BuildRequest {
        api_keys: api_keys("test-key", "tron-key"),
        ..harness.request(&ChainId::ALL)
    };
    let keyed = harness.factory.build(&request).await;
    assert_eq!(keyed.built(), ChainId::ALL.to_vec());
    for chain in [ChainId::Polygon, ChainId::Bsc] {
        assert!(rendered(&keyed, chain).unwrap().contains("indexed_history: true"), "{chain}");
    }
    for chain in ChainId::ALL {
        let printed = rendered(&keyed, chain).unwrap();
        assert!(!printed.contains("test-key"), "{chain}: {printed}");
        assert!(!printed.contains("tron-key"), "{chain}: {printed}");
    }
}

#[tokio::test]
async fn a_blank_api_key_is_not_a_reason_to_lose_a_chain() {
    let harness = Harness::new().unwrap();
    let request = BuildRequest { api_keys: api_keys("   ", ""), ..harness.request(&ChainId::ALL) };

    let set = harness.factory.build(&request).await;
    assert_eq!(set.built(), ChainId::ALL.to_vec());
    assert!(rendered(&set, ChainId::Polygon).unwrap().contains("indexed_history: false"));
}

#[tokio::test]
async fn a_build_with_its_own_proxy_leaves_process_egress_alone() {
    let harness = Harness::new().unwrap();
    assert_eq!(harness.network.proxy(), None);

    let request = BuildRequest {
        proxy: Some(ProxySetting::Socks5h { url: "socks5h://127.0.0.1:9050".to_owned() }),
        ..harness.request(&ChainId::ALL)
    };
    let set = harness.factory.build(&request).await;
    assert_eq!(set.built(), ChainId::ALL.to_vec());
    assert_eq!(harness.network.proxy(), None, "the build did not change the shared route");

    let older = harness.request(&ChainId::ALL);
    let prepared = harness.network.prepare_proxy(Some(ProxySetting::System)).unwrap();
    harness.network.apply_proxy(prepared);
    assert_eq!(harness.factory.build(&older).await.built(), ChainId::ALL.to_vec());
    assert_eq!(
        harness.network.proxy(),
        Some(ProxySetting::System),
        "an older request cannot restore its route"
    );
}

#[tokio::test]
async fn an_egress_route_the_client_refuses_costs_every_chain() {
    let harness = Harness::new().unwrap();
    let request = BuildRequest {
        proxy: Some(ProxySetting::Socks5h { url: String::new() }),
        ..harness.request(&ChainId::ALL)
    };

    let set = harness.factory.build(&request).await;
    assert!(set.built().is_empty(), "{:?}", set.built());
    assert_eq!(skipped(&set), every_chain_skipped_for(SkipReason::BuildFailed));
    assert_eq!(harness.network.proxy(), None, "the refused route was not installed");
}

/// The registry skips a row it cannot read, so the row reaches no pool.
#[tokio::test]
async fn an_unreadable_endpoint_row_costs_no_chain() {
    let harness = Harness::new().unwrap();
    harness
        .network
        .registry()
        .load_overrides([(ChainId::Solana, EndpointOverride::new("not an address"))]);

    let set = harness.build(&ChainId::ALL).await;
    assert!(skipped(&set).is_empty());
    assert_eq!(set.built(), ChainId::ALL.to_vec());
}

#[tokio::test]
async fn the_monero_adapter_starts_at_the_requested_restore_height() {
    let harness = Harness::new().unwrap();
    let fresh = harness.build(&[ChainId::Monero]).await;
    assert!(rendered(&fresh, ChainId::Monero).unwrap().contains("synced_height: 0"));

    let request = BuildRequest {
        restore_heights: BTreeMap::from([(ChainId::Monero, RESTORE_HEIGHT)]),
        ..harness.request(&[ChainId::Monero])
    };
    let seeded = harness.factory.build(&request).await;
    assert!(
        rendered(&seeded, ChainId::Monero)
            .unwrap()
            .contains(&format!("synced_height: {RESTORE_HEIGHT}"))
    );
}

#[tokio::test]
async fn every_adapter_holds_the_requests_cursor_store_until_it_is_dropped() {
    let harness = Harness::new().unwrap();
    let before = Arc::strong_count(&harness.cursors);
    for chain in ChainId::ALL {
        let set = harness.build(&[chain]).await;

        assert_eq!(set.built(), vec![chain]);
        assert!(Arc::strong_count(&harness.cursors) > before, "{chain} holds another store");
        drop(set);
        assert_eq!(Arc::strong_count(&harness.cursors), before, "{chain}");
    }
}

#[tokio::test]
async fn the_factory_prints_nothing_but_whether_it_is_attached() {
    assert_eq!(format!("{:?}", AdapterFactory::new()), "AdapterFactory { is_attached: false, .. }");
    let harness = Harness::new().unwrap();
    assert_eq!(format!("{:?}", harness.factory), "AdapterFactory { is_attached: true, .. }");
}

/// The setting is a `TronGrid` credential, so a TRON node anyone else runs, curated or
/// added by the user, never receives it.
#[tokio::test]
async fn the_trongrid_key_reaches_no_other_tron_node() {
    const TRONGRID_KEY: &str = "trongrid-secret";
    const MAINNET_BLOCK_ZERO: &str =
        r#"{"blockID":"00000000000000001ebf88508a03865c71d452e25f4d51194196a1d22b6653dc"}"#;
    const NOW_BLOCK: &str = r#"{"blockID":"000000000515a651af7c721ebc0c55d84f973b989508d8f40ba10affa980fd3e","block_header":{"raw_data":{"number":85304913,"timestamp":1786585806000}}}"#;

    let (url, server) =
        serve(vec![("200 OK", MAINNET_BLOCK_ZERO), ("200 OK", NOW_BLOCK)]).await.unwrap();

    let harness = Harness::new().unwrap();
    let registry = harness.network.registry();
    let mut rows: Vec<(ChainId, EndpointOverride)> = registry
        .defaults(ChainId::Tron)
        .unwrap()
        .endpoints
        .iter()
        .map(|entry| {
            let row = EndpointOverride { is_enabled: false, ..EndpointOverride::new(&entry.url) };
            (ChainId::Tron, row)
        })
        .collect();
    rows.push((ChainId::Tron, EndpointOverride::new(url)));
    registry.load_overrides(rows);

    let request = BuildRequest {
        api_keys: BTreeMap::from([(
            KeyService::TronGrid,
            SecretText::new(TRONGRID_KEY.to_owned()),
        )]),
        ..harness.request(&[ChainId::Tron])
    };
    let set = harness.factory.build(&request).await;
    let tron = adapter(&set, ChainId::Tron).unwrap();
    let deadline = harness.network.clock().deadline_in(Duration::from_secs(20));
    let events = FakeEventSink::new();
    let pass = timeout(Duration::from_secs(30), tron.sync(deadline, &events)).await;
    assert!(pass.is_ok(), "the sync pass did not finish");

    let requests = timeout(Duration::from_secs(30), server).await.unwrap().unwrap().unwrap();
    let tip = requests.last().unwrap();
    assert!(tip.starts_with("POST /wallet/getblock "), "{tip}");
    for request in &requests {
        assert!(!request.contains(TRONGRID_KEY), "{request}");
    }
}
