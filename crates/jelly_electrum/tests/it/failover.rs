use std::sync::Arc;
use std::time::Duration;

use jelly_core::{ChainId, Rejection, RejectionKind, RetryPolicy, Transport, test_util::FakeClock};
use jelly_electrum::{
    ElectrumClient, ElectrumError, Method, ProtocolVersion, classify, classify_broadcast,
    test_util::{
        Answer, ENDPOINT_URL, FakeElectrum, bitcoin_chain, bitcoin_genesis_header, endpoint_label,
        foreign_genesis_header,
    },
};
use jelly_net::{Endpoint, HealthClass, NetError, NetRuntime, Outcome, PacerPolicy, Pool};

use crate::common::{FEE_HISTOGRAM, histogram_buckets, old_server_version, quoted, raw, tx};

fn runtime() -> (Arc<NetRuntime>, Arc<FakeClock>) {
    let clock = Arc::new(FakeClock::default());
    let runtime =
        NetRuntime::with_pacing(Arc::<FakeClock>::clone(&clock), PacerPolicy::UNTHROTTLED);
    (runtime, clock)
}

/// A pool of one endpoint over the in-memory server, built on `runtime` the way a chain
/// crate builds one and running the real chain guard.
fn pool_on(fake: &FakeElectrum, runtime: &NetRuntime) -> Option<Arc<Pool<ElectrumClient>>> {
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::ElectrumSsl, ENDPOINT_URL).ok()?;
    Some(Arc::new(runtime.pool(
        ChainId::Bitcoin,
        Transport::ElectrumSsl,
        Arc::new(fake.connector(bitcoin_chain())),
        vec![endpoint],
    )))
}

fn pool_over(fake: &FakeElectrum) -> Option<Arc<Pool<ElectrumClient>>> {
    pool_on(fake, &runtime().0)
}

fn rejected(retry: RetryPolicy) -> ElectrumError {
    ElectrumError::Rejected {
        endpoint: endpoint_label(),
        rejection: Rejection::new(RejectionKind::Unclassified, retry, "refused"),
    }
}

mod classification {
    use super::*;

    #[test]
    fn a_read_stops_the_walk_only_when_the_node_made_up_its_mind() {
        for retry in
            [RetryPolicy::DidNotLand, RetryPolicy::FinalEverywhere, RetryPolicy::AlreadyLanded]
        {
            let error = NetError::from(rejected(retry));
            assert_eq!(classify(&error), Outcome::Fail, "{}", retry.as_str());
        }
        let error = NetError::from(rejected(RetryPolicy::TryAnotherEndpoint));
        assert_eq!(classify(&error), Outcome::Rotate);
    }

    #[test]
    fn a_method_the_negotiated_protocol_lacks_stops_the_walk() {
        let error = NetError::from(ElectrumError::MethodUnavailable {
            endpoint: endpoint_label(),
            method: Method::MempoolInfo,
            since: ProtocolVersion::V1_6,
            negotiated: ProtocolVersion::V1_4,
        });
        assert_eq!(classify(&error), Outcome::Fail);
    }

    #[test]
    fn a_transport_failure_rotates() {
        let error = NetError::from(ElectrumError::Closed { endpoint: endpoint_label() });
        assert_eq!(classify(&error), Outcome::Rotate);
        assert_eq!(classify_broadcast(&error), Outcome::Rotate);
    }

    #[test]
    fn a_broadcast_treats_every_refusal_as_the_answer() {
        for retry in RetryPolicy::ALL {
            let error = NetError::from(rejected(retry));
            assert_eq!(classify_broadcast(&error), Outcome::Accept, "{}", retry.as_str());
        }
    }

    #[test]
    fn the_typed_failure_survives_the_trip_through_net_error() {
        let original = ElectrumError::BatchLength {
            endpoint: endpoint_label(),
            method: Method::ListUnspent,
            requested: 3,
            received: 1,
        };
        let text = original.to_string();
        let recovered = ElectrumError::from(NetError::from(original));
        assert_eq!(recovered.to_string(), text);
        assert!(
            matches!(recovered, ElectrumError::BatchLength { requested: 3, received: 1, .. }),
            "{recovered}"
        );
    }

    #[test]
    fn a_plain_network_failure_is_not_mistaken_for_an_electrum_one() {
        let net = NetError::Timeout { endpoint: endpoint_label(), elapsed_millis: 10 };
        let wrapped = ElectrumError::from(net);
        assert!(matches!(wrapped, ElectrumError::Net(NetError::Timeout { .. })), "{wrapped}");
        assert_eq!(classify(&NetError::from(wrapped)), Outcome::Rotate);
    }
}

mod walking {
    use super::*;

    async fn bucket_count(pool: &Pool<ElectrumClient>) -> Result<usize, ElectrumError> {
        pool.call("fee_histogram", classify, async |client: Arc<ElectrumClient>| {
            client.fee_histogram().await.map(|buckets| buckets.len()).map_err(NetError::from)
        })
        .await
        .map_err(ElectrumError::from)
    }

    #[tokio::test(start_paused = true)]
    async fn the_pool_answers_through_the_typed_api() {
        let fake = FakeElectrum::new();
        fake.serves_bitcoin();
        fake.always(Method::FeeHistogram, Answer::Result(raw(FEE_HISTOGRAM, "/result")));
        let pool = pool_over(&fake).expect("a pool over one endpoint");
        assert_eq!(bucket_count(&pool).await.expect("the endpoint answers"), histogram_buckets());
        assert_eq!(pool.endpoints().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_no_endpoint_answers_becomes_the_aggregate() {
        let fake = FakeElectrum::new();
        fake.serves_bitcoin();
        fake.always(
            Method::FeeHistogram,
            Answer::Error { code: -32601, message: "unknown method".to_owned() },
        );
        let pool = pool_over(&fake).expect("a pool over one endpoint");
        let error = bucket_count(&pool).await.expect_err("the only endpoint refuses");
        assert!(matches!(error, ElectrumError::Net(NetError::AllFailed { .. })), "{error}");
    }

    /// Most public servers negotiate below 1.6, so a walk that blamed each one
    /// for lacking `mempool.get_info` would back off the whole pool at once.
    #[tokio::test(start_paused = true)]
    async fn a_server_below_the_protocol_a_method_needs_keeps_its_health() {
        let fake = FakeElectrum::new();
        fake.serves_bitcoin();
        fake.always(Method::ServerVersion, old_server_version());
        let (runtime, _) = runtime();
        let pool = pool_on(&fake, &runtime).expect("a pool over one endpoint");
        let error = pool
            .call("mempool_info", classify, async |client: Arc<ElectrumClient>| {
                client.mempool_info().await.map_err(NetError::from)
            })
            .await
            .map_err(ElectrumError::from)
            .expect_err("the server negotiated 1.4");
        assert!(matches!(error, ElectrumError::MethodUnavailable { .. }), "{error}");

        let url = pool.endpoints().first().map(|endpoint| endpoint.url().to_owned());
        let health = runtime.health().entry(ChainId::Bitcoin, &url.unwrap_or_default());
        assert_eq!(health.health.failures, 0, "{health:?}");
        assert_ne!(health.class, HealthClass::Quarantined, "{health:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_endpoint_on_the_wrong_chain_is_disqualified() {
        let fake = FakeElectrum::new();
        fake.always(Method::BlockHeader, Answer::Result(quoted(&foreign_genesis_header())));
        let (runtime, clock) = runtime();
        let pool = pool_on(&fake, &runtime).expect("a pool over one endpoint");
        let first = bucket_count(&pool).await.expect_err("the endpoint is on another chain");
        assert!(matches!(first, ElectrumError::Net(NetError::AllFailed { .. })), "{first}");

        // Past the pacer's backoff, so only the disqualification can keep the
        // endpoint out of the walk.
        clock.advance(Duration::from_secs(1));
        let sockets = fake.sockets_opened();
        let second = bucket_count(&pool).await.expect_err("and stays disqualified");
        assert!(matches!(second, ElectrumError::Net(NetError::NoneReady { .. })), "{second}");
        assert_eq!(fake.sockets_opened(), sockets, "a disqualified endpoint is never dialled");
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_attempt_broadcast_keeps_every_rejection() {
        for (message, kind, retry) in [
            ("txn-already-in-mempool", RejectionKind::AlreadyKnown, RetryPolicy::AlreadyLanded),
            ("txn-mempool-conflict", RejectionKind::DoubleSpend, RetryPolicy::TryAnotherEndpoint),
            (
                "the server is having a bad day",
                RejectionKind::Unclassified,
                RetryPolicy::TryAnotherEndpoint,
            ),
        ] {
            let fake = FakeElectrum::new();
            fake.serves_bitcoin();
            fake.always(
                Method::Broadcast,
                Answer::Error { code: -26, message: message.to_owned() },
            );
            let pool = pool_over(&fake).expect("a pool over one endpoint");
            let tx = Arc::new(tx());
            let error = pool
                .broadcast_once("broadcast", classify_broadcast, |client: Arc<ElectrumClient>| {
                    let tx = Arc::clone(&tx);
                    async move { client.broadcast(&tx).await.map_err(NetError::from) }
                })
                .await
                .map_err(ElectrumError::from)
                .expect_err("the node refuses");
            let rejection = error.rejection().unwrap_or_else(|| panic!("{message}: {error}"));
            assert_eq!(rejection.kind, kind, "{message}");
            assert_eq!(rejection.retry, retry, "{message}");
        }
    }

    /// The pool never identifies a client it rebuilds for an endpoint.
    ///
    /// It proves an endpoint once per session and drops its cached client on any
    /// rotating failure, so only the rebuilt client's own proof keeps a foreign
    /// chain out.
    #[tokio::test(start_paused = true)]
    async fn a_client_rebuilt_after_a_failure_proves_its_chain_before_serving() {
        let fake = FakeElectrum::new();
        fake.serves_bitcoin();
        fake.always(Method::FeeHistogram, Answer::Result(raw(FEE_HISTOGRAM, "/result")));
        let (runtime, clock) = runtime();
        let pool = pool_on(&fake, &runtime).expect("a pool over one endpoint");
        assert_eq!(
            bucket_count(&pool).await.expect("the proved endpoint answers"),
            histogram_buckets()
        );

        fake.on(Method::FeeHistogram, Answer::Error { code: 1, message: "busy".to_owned() });
        bucket_count(&pool).await.expect_err("one failure that rotates");

        // The host now serves another chain.
        fake.always(Method::ServerFeatures, Answer::Result("{}".to_owned()));
        fake.always(Method::BlockHeader, Answer::Result(quoted(&foreign_genesis_header())));
        clock.advance(Duration::from_secs(1));
        fake.forget_calls();
        let answered = bucket_count(&pool).await;
        let methods: Vec<Method> = fake.calls().iter().map(|call| call.method).collect();
        assert!(answered.is_err(), "a foreign chain answered {answered:?}, calls {methods:?}");
        assert!(
            methods.contains(&Method::BlockHeader),
            "block zero was never asked for: {methods:?}"
        );
        assert!(
            !methods.contains(&Method::FeeHistogram),
            "the foreign chain was read before its proof: {methods:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_socket_is_proved_before_any_request_reaches_it() {
        let fake = FakeElectrum::new();
        fake.on(Method::BlockHeader, Answer::Result(quoted(&bitcoin_genesis_header())));
        fake.always(Method::FeeHistogram, Answer::Result(raw(FEE_HISTOGRAM, "/result")));
        let pool = pool_over(&fake).expect("a pool over one endpoint");
        bucket_count(&pool).await.expect("the endpoint answers");

        let calls = fake.calls();
        let header = calls
            .iter()
            .position(|call| call.method == Method::BlockHeader)
            .expect("block zero was asked for");
        let work = calls
            .iter()
            .position(|call| call.method == Method::FeeHistogram)
            .expect("the histogram was asked for");
        assert!(header < work, "{calls:?}");
    }
}
