use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{ChainId, Clock, EndpointLabel, FailureKind, Transport, test_util::FakeClock};
use jelly_net::{
    Connect, Endpoint, EndpointOrigin, HealthClass, Identity, NetError, NetRuntime, Outcome,
    PacerPolicy, Pool, Standing, TimeoutPolicy, json_rpc_outcome,
};
use tokio::sync::Notify;

use crate::common::{
    ENDPOINT_A, ENDPOINT_B, ENDPOINT_C, FakeConnection, endpoint, harness, harness_with, rotate,
};

const fn fail(_: &NetError) -> Outcome {
    Outcome::Fail
}

const fn accept(_: &NetError) -> Outcome {
    Outcome::Accept
}

const fn skip(_: &NetError) -> Outcome {
    Outcome::Skip
}

fn refused() -> NetError {
    NetError::protocol(&EndpointLabel::new("https", "a.example", None), "refused by the test")
}

#[tokio::test(start_paused = true)]
async fn the_walk_returns_the_first_endpoint_that_answers() {
    let harness =
        harness(&[ENDPOINT_A, ENDPOINT_B, ENDPOINT_C]).expect("a pool over three endpoints");
    harness.connector.refused_urls.lock().extend([ENDPOINT_A.to_owned(), ENDPOINT_B.to_owned()]);

    let answered = harness
        .pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect("the third endpoint answers");

    assert_eq!(answered, ENDPOINT_C);
    assert_eq!(
        *harness.connector.connects.lock(),
        vec![ENDPOINT_A.to_owned(), ENDPOINT_B.to_owned(), ENDPOINT_C.to_owned()]
    );
    assert_eq!(harness.health.entry(harness.pool.chain(), ENDPOINT_C).class, HealthClass::Healthy);
    assert_eq!(harness.health.entry(harness.pool.chain(), ENDPOINT_A).health.failures, 1);
}

#[tokio::test(start_paused = true)]
async fn a_deterministic_rejection_stops_at_the_first_endpoint() {
    let harness =
        harness(&[ENDPOINT_A, ENDPOINT_B, ENDPOINT_C]).expect("a pool over three endpoints");
    let calls = AtomicU32::new(0);

    let error = harness
        .pool
        .call("send", fail, |_connection| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err::<(), _>(refused()) }
        })
        .await
        .expect_err("the classifier stops the walk");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(error, NetError::Protocol { .. }));
    let state = harness.health.entry(harness.pool.chain(), ENDPOINT_A).health;
    assert_eq!(state.failures, 0, "a deterministic refusal is not the endpoint's fault");
    assert_eq!(state.successes, 0);
}

#[tokio::test(start_paused = true)]
async fn an_accepted_answer_credits_the_endpoint() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_B]).expect("a pool over two endpoints");

    let error = harness
        .pool
        .call("send", accept, async |_connection| Err::<(), _>(refused()))
        .await
        .expect_err("the answer is an error the caller asked for");

    assert!(matches!(error, NetError::Protocol { .. }));
    assert_eq!(harness.health.entry(harness.pool.chain(), ENDPOINT_A).health.successes, 1);
    assert_eq!(harness.connector.connects.lock().len(), 1);
}

/// A host that refuses only one kind of query keeps its standing, so a request it
/// does serve still reaches it straight away.
#[tokio::test(start_paused = true)]
async fn a_skipped_endpoint_is_passed_over_without_a_penalty() {
    let policy = PacerPolicy {
        backoff_floor: Duration::from_secs(5),
        backoff_ceiling: Duration::from_secs(5),
        ..PacerPolicy::UNTHROTTLED
    };
    let harness = harness_with(&[ENDPOINT_A, ENDPOINT_B], policy).expect("a pool over two");

    let answered = harness
        .pool
        .call("logs", skip, async |connection| {
            if connection.url == ENDPOINT_A { Err(refused()) } else { Ok(connection.url.clone()) }
        })
        .await
        .expect("the second endpoint answers");
    assert_eq!(answered, ENDPOINT_B);
    let state = harness.health.entry(harness.pool.chain(), ENDPOINT_A).health;
    assert_eq!(state.failures, 0, "a skipped endpoint is not blamed");

    let answered = harness
        .pool
        .call("balance", rotate, async |connection| {
            if connection.url == ENDPOINT_B { Err(refused()) } else { Ok(connection.url.clone()) }
        })
        .await
        .expect("the skipped endpoint is not cooling down");
    assert_eq!(answered, ENDPOINT_A);
}

#[tokio::test(start_paused = true)]
async fn a_reported_attempt_names_the_reason_and_not_only_its_class() {
    let harness = harness(&[ENDPOINT_A]).expect("a pool over one endpoint");
    harness.connector.refused_urls.lock().push(ENDPOINT_A.to_owned());

    let error = harness
        .pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("the only endpoint refuses");

    let NetError::AllFailed { attempts, .. } = error else {
        panic!("an exhausted walk reports every attempt");
    };
    let attempt = attempts.first().expect("one attempt");
    assert_eq!(attempt.kind, FailureKind::Connect);
    assert!(attempt.message.contains("a.example"), "{}", attempt.message);
    assert!(attempt.message.contains("connect"), "{}", attempt.message);
    assert!(attempt.message.contains("refused"), "{}", attempt.message);
}

#[tokio::test(start_paused = true)]
async fn the_walk_deadline_stops_the_rotation_early() {
    let harness =
        harness(&[ENDPOINT_A, ENDPOINT_B, ENDPOINT_C]).expect("a pool over three endpoints");
    // The whole walk gets forty five seconds and every dial burns twenty.
    *harness.connector.advance_on_connect.lock() = Duration::from_secs(20);
    harness.connector.refused_urls.lock().extend([
        ENDPOINT_A.to_owned(),
        ENDPOINT_B.to_owned(),
        ENDPOINT_C.to_owned(),
    ]);

    let error = harness
        .pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("no endpoint answers");

    let NetError::AllFailed { attempts, .. } = &error else {
        panic!("expected three reported attempts, got {error}");
    };
    assert_eq!(attempts.len(), 3);
    assert_eq!(harness.connector.connects.lock().len(), 3);

    // A fourth endpoint does not fit in the remaining budget.
    let four: Vec<_> = [ENDPOINT_A, ENDPOINT_B, ENDPOINT_C, "https://d.example"]
        .iter()
        .map(|url| endpoint(url).expect("a canonical url"))
        .collect();
    let pool = harness.pool_over(four);
    harness.connector.connects.lock().clear();
    harness.connector.refused_urls.lock().push("https://d.example".to_owned());
    let _ = pool.call("head", rotate, async |connection| Ok(connection.url.clone())).await;
    assert_eq!(harness.connector.connects.lock().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_hanging_attempt_times_out_and_rotates() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_B]).expect("a pool over two endpoints");

    let answered = harness
        .pool
        .call("head", rotate, async |connection| {
            if connection.url == ENDPOINT_A {
                pending::<()>().await;
            }
            Ok(connection.url.clone())
        })
        .await
        .expect("the second endpoint answers");

    assert_eq!(answered, ENDPOINT_B);
    let first = harness.health.entry(harness.pool.chain(), ENDPOINT_A).health;
    assert_eq!(first.failures, 1);
    assert_eq!(first.last_error.as_deref(), Some("https://a.example: timed out after 15000 ms"));
}

#[tokio::test(start_paused = true)]
async fn a_slow_dial_leaves_the_call_only_what_is_left_of_the_attempt() {
    let harness = harness(&[ENDPOINT_A]).expect("a pool over one endpoint");
    *harness.connector.advance_on_connect.lock() = Duration::from_secs(12);

    let error = harness
        .pool
        .call("head", rotate, async |_connection| pending::<Result<String, NetError>>().await)
        .await
        .expect_err("the only endpoint never answers");

    let NetError::AllFailed { attempts, .. } = error else {
        panic!("an exhausted walk reports every attempt");
    };
    let attempt = attempts.first().expect("one attempt");
    assert_eq!(attempt.kind, FailureKind::Timeout);
    assert!(attempt.message.contains("3000 ms"), "{}", attempt.message);
}

#[tokio::test(start_paused = true)]
async fn an_identity_check_that_never_answers_times_out_and_rotates() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_B]).expect("a pool over two endpoints");
    harness.connector.silent_identity_urls.lock().push(ENDPOINT_A.to_owned());

    let answered = harness
        .pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect("the second endpoint answers");

    assert_eq!(answered, ENDPOINT_B);
    let first = harness.health.entry(harness.pool.chain(), ENDPOINT_A).health;
    assert_eq!(first.failures, 1);
    assert_eq!(first.last_error.as_deref(), Some("https://a.example: timed out after 15000 ms"));
}

#[tokio::test(start_paused = true)]
async fn an_endpoint_on_the_wrong_network_is_never_tried_again() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_B]).expect("a pool over two endpoints");
    harness.connector.wrong_network_urls.lock().push(ENDPOINT_A.to_owned());

    let answered = harness
        .pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect("the second endpoint answers");
    assert_eq!(answered, ENDPOINT_B);

    // The clock moves past the cooldown the mismatch earned, so once b fails only the
    // disqualification keeps a out of the walk.
    harness.connector.clock.advance(Duration::from_secs(1));
    harness.connector.identity_checks.lock().clear();
    harness.connector.connects.lock().clear();
    let error = harness
        .pool
        .call("head", rotate, async |connection| {
            if connection.url == ENDPOINT_B { Err(refused()) } else { Ok(connection.url.clone()) }
        })
        .await
        .expect_err("b refuses and a is not tried");

    let NetError::AllFailed { attempts, .. } = error else {
        panic!("an exhausted walk reports every attempt");
    };
    let hosts: Vec<&str> = attempts.iter().map(|attempt| attempt.endpoint.host()).collect();
    assert_eq!(hosts, ["b.example"]);
    assert_eq!(*harness.connector.connects.lock(), Vec::<String>::new(), "b is still cached");
    assert_eq!(*harness.connector.identity_checks.lock(), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn a_mismatch_rotates_even_when_the_classifier_would_stop() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_B]).expect("a pool over two endpoints");
    harness.connector.wrong_network_urls.lock().push(ENDPOINT_A.to_owned());

    // `fail` would end the walk at the first failure, but an identity mismatch is not
    // the caller's to accept.
    let answered = harness
        .pool
        .call("head", fail, async |connection| Ok(connection.url.clone()))
        .await
        .expect("the second endpoint answers");

    assert_eq!(answered, ENDPOINT_B);
}

#[tokio::test(start_paused = true)]
async fn the_identity_check_runs_once_per_endpoint() {
    let harness = harness(&[ENDPOINT_A]).expect("a pool over one endpoint");

    for _ in 0..3u8 {
        let _ =
            harness.pool.call("head", rotate, async |connection| Ok(connection.url.clone())).await;
    }

    assert_eq!(*harness.connector.identity_checks.lock(), vec![ENDPOINT_A.to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn an_empty_pool_reports_no_endpoints() {
    let harness = harness(&[]).expect("an empty pool");

    let error = harness
        .pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("nothing to try");

    assert!(matches!(error, NetError::NoEndpoints { .. }));
}

#[tokio::test(start_paused = true)]
async fn a_broadcast_tries_exactly_one_endpoint() {
    let harness =
        harness(&[ENDPOINT_A, ENDPOINT_B, ENDPOINT_C]).expect("a pool over three endpoints");
    harness.connector.refused_urls.lock().push(ENDPOINT_A.to_owned());

    let error = harness
        .pool
        .broadcast_once("broadcast", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("the one endpoint tried refused the dial");

    let NetError::AllFailed { attempts, .. } = &error else {
        panic!("expected one attempt, got {error}");
    };
    assert_eq!(attempts.len(), 1);
    assert_eq!(harness.connector.connects.lock().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_quarantined_pin_falls_behind_an_untried_endpoint() {
    let harness = harness(&[]).expect("an empty pool");
    let first = endpoint(ENDPOINT_A).expect("a canonical url").with_origin(EndpointOrigin::Pinned);
    let second = endpoint(ENDPOINT_B).expect("a canonical url");
    let pool = harness.pool_over(vec![first.clone(), second]);

    let answered = pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect("the pin answers while it is healthy");
    assert_eq!(answered, ENDPOINT_A);

    for _ in 0..3u8 {
        harness
            .health
            .record_failure(&first, &NetError::timed_out(first.label(), Duration::from_secs(15)));
    }
    assert_eq!(harness.health.entry(pool.chain(), ENDPOINT_A).class, HealthClass::Quarantined);
    assert_eq!(harness.health.entry(pool.chain(), ENDPOINT_B).class, HealthClass::Unknown);

    harness.connector.connects.lock().clear();
    let answered = pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect("b answers");

    assert_eq!(answered, ENDPOINT_B);
    assert_eq!(*harness.connector.connects.lock(), vec![ENDPOINT_B.to_owned()]);
}

/// A falls back, b serves nothing, and every other endpoint is preferred.
fn ranked(endpoint: &Endpoint) -> Standing {
    match endpoint.url() {
        ENDPOINT_A => Standing::Fallback,
        ENDPOINT_B => Standing::Excluded,
        _ => Standing::Preferred,
    }
}

const fn nowhere(_: &Endpoint) -> Standing {
    Standing::Excluded
}

#[tokio::test(start_paused = true)]
async fn a_ranked_walk_asks_the_preferred_endpoint_then_the_fallback_and_never_an_excluded_one() {
    let harness =
        harness(&[ENDPOINT_A, ENDPOINT_B, ENDPOINT_C]).expect("a pool over three endpoints");

    let error = harness
        .pool
        .call_ranked("head", rotate, ranked, async |_connection| Err::<String, _>(refused()))
        .await
        .expect_err("every endpoint asked refuses");

    let NetError::AllFailed { attempts, .. } = error else {
        panic!("an exhausted walk reports every attempt");
    };
    let hosts: Vec<&str> = attempts.iter().map(|attempt| attempt.endpoint.host()).collect();
    assert_eq!(hosts, ["c.example", "a.example"]);
    assert_eq!(
        *harness.connector.connects.lock(),
        vec![ENDPOINT_C.to_owned(), ENDPOINT_A.to_owned()]
    );
}

#[tokio::test(start_paused = true)]
async fn a_ranked_broadcast_tries_only_the_preferred_endpoint() {
    let harness =
        harness(&[ENDPOINT_A, ENDPOINT_B, ENDPOINT_C]).expect("a pool over three endpoints");

    let error = harness
        .pool
        .broadcast_once_ranked("broadcast", rotate, ranked, async |connection| {
            if connection.url == ENDPOINT_C { Err(refused()) } else { Ok(connection.url.clone()) }
        })
        .await
        .expect_err("the one endpoint tried refused");

    let NetError::AllFailed { attempts, .. } = error else {
        panic!("a failed broadcast reports its attempt");
    };
    let hosts: Vec<&str> = attempts.iter().map(|attempt| attempt.endpoint.host()).collect();
    assert_eq!(hosts, ["c.example"]);
    assert_eq!(*harness.connector.connects.lock(), vec![ENDPOINT_C.to_owned()]);
}

/// A fallback that has answered before does not keep the requests a preferred endpoint
/// was never asked for, while an unranked walk still puts the proven endpoint first.
#[tokio::test(start_paused = true)]
async fn an_untried_preferred_endpoint_goes_before_a_fallback_that_has_answered() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_C]).expect("a pool over two endpoints");
    let proven = endpoint(ENDPOINT_A).expect("a canonical url");
    harness.health.record_success(&proven, Duration::from_millis(50));
    let echo = async |connection: Arc<FakeConnection>| Ok(connection.url.clone());

    let unranked = harness.pool.call("head", rotate, echo).await.expect("a answers");
    assert_eq!(unranked, ENDPOINT_A);

    let answered = harness.pool.call_ranked("head", rotate, ranked, echo).await.expect("c answers");
    assert_eq!(answered, ENDPOINT_C);
}

#[tokio::test(start_paused = true)]
async fn a_failing_preferred_endpoint_falls_behind_a_healthy_fallback() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_C]).expect("a pool over two endpoints");
    let failing = endpoint(ENDPOINT_C).expect("a canonical url");
    harness.health.record_failure(&failing, &refused());
    assert_eq!(harness.health.entry(harness.pool.chain(), ENDPOINT_C).class, HealthClass::Degraded);

    let answered = harness
        .pool
        .call_ranked("head", rotate, ranked, async |connection| Ok(connection.url.clone()))
        .await
        .expect("the fallback answers");

    assert_eq!(answered, ENDPOINT_A);
    assert_eq!(*harness.connector.connects.lock(), vec![ENDPOINT_A.to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn a_walk_that_excludes_every_endpoint_sends_nothing() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_B]).expect("a pool over two endpoints");

    let error = harness
        .pool
        .call_ranked("history", rotate, nowhere, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("no endpoint serves the request");

    assert!(matches!(error, NetError::NoEndpoints { .. }), "{error}");
    assert!(harness.connector.connects.lock().is_empty(), "nothing was sent");
}

/// Answers the identity question with a JSON-RPC error for one host, as a node that
/// speaks another protocol answers `eth_chainId`, and confirms every other host.
#[derive(Debug)]
struct RefusingIdentity {
    refusing: &'static str,
}

#[async_trait]
impl Connect<FakeConnection> for RefusingIdentity {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<FakeConnection>, NetError> {
        Ok(Arc::new(FakeConnection { url: endpoint.url().to_owned() }))
    }

    async fn identify(
        &self,
        endpoint: &Endpoint,
        _connection: &FakeConnection,
    ) -> Result<Identity, NetError> {
        if endpoint.url() == self.refusing {
            return Err(NetError::Rpc {
                endpoint: endpoint.label().clone(),
                code: -32601,
                message: "Method not found".to_owned(),
            });
        }
        Ok(Identity::confirmed("chain id 1"))
    }
}

const fn by_rpc_code(error: &NetError) -> Outcome {
    if let NetError::Rpc { code, .. } = *error { json_rpc_outcome(code) } else { Outcome::Rotate }
}

/// The classifier judges the caller's request. A host that cannot answer the identity
/// question has proved nothing about the request, so the walk must move on to a host
/// that can.
#[tokio::test(start_paused = true)]
async fn an_identity_check_that_errors_rotates_even_when_the_classifier_would_stop() {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let runtime = NetRuntime::with_pacing(clock, PacerPolicy::UNTHROTTLED);
    let pool = runtime.pool(
        ChainId::Ethereum,
        Transport::HttpJsonrpc,
        Arc::new(RefusingIdentity { refusing: ENDPOINT_A }),
        vec![
            endpoint(ENDPOINT_A).expect("a canonical url"),
            endpoint(ENDPOINT_B).expect("a canonical url"),
        ],
    );

    let answered =
        pool.call("head", by_rpc_code, async |connection| Ok(connection.url.clone())).await;

    assert_eq!(
        answered.as_ref().ok().map(String::as_str),
        Some(ENDPOINT_B),
        "the walk stopped at the host that failed its identity check: {answered:?}"
    );
}

/// Confirms the first identity check late and finds another network on every later one.
///
/// The first check is held open until the test releases it, as a load-balanced host with
/// one misconfigured backend can answer.
#[derive(Debug, Default)]
struct SlowConfirmationThenMismatch {
    checks: AtomicU32,
    first_check_started: Notify,
    release_first_check: Notify,
}

#[async_trait]
impl Connect<FakeConnection> for SlowConfirmationThenMismatch {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<FakeConnection>, NetError> {
        Ok(Arc::new(FakeConnection { url: endpoint.url().to_owned() }))
    }

    async fn identify(
        &self,
        _endpoint: &Endpoint,
        _connection: &FakeConnection,
    ) -> Result<Identity, NetError> {
        if self.checks.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first_check_started.notify_one();
            self.release_first_check.notified().await;
            return Ok(Identity::confirmed("chain id 1"));
        }
        Ok(Identity::Mismatch { expected: "1".to_owned(), found: "56".to_owned() })
    }
}

/// A mismatch disqualifies the endpoint for the rest of the session.
///
/// `Connect::identify` promises that, so a walk whose own check was still in flight when
/// another walk found the wrong network must not undo it when its check comes back
/// confirmed.
#[tokio::test(start_paused = true)]
async fn a_late_confirmation_never_readmits_an_endpoint_disqualified_for_another_network() {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let runtime = NetRuntime::with_pacing(clock, PacerPolicy::UNTHROTTLED);
    let connector = Arc::new(SlowConfirmationThenMismatch::default());
    let pool: Arc<Pool<FakeConnection>> = Arc::new(runtime.pool(
        ChainId::Ethereum,
        Transport::HttpJsonrpc,
        Arc::<SlowConfirmationThenMismatch>::clone(&connector),
        vec![endpoint(ENDPOINT_A).expect("a canonical url")],
    ));
    let echo = async |connection: Arc<FakeConnection>| Ok(connection.url.clone());

    let slow = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move { pool.call("head", rotate, echo).await }
    });
    connector.first_check_started.notified().await;

    let mismatched = pool.call("head", rotate, echo).await;
    assert!(
        matches!(mismatched, Err(NetError::AllFailed { .. })),
        "the second check found another network: {mismatched:?}"
    );

    connector.release_first_check.notify_one();
    let _ = slow.await.expect("the slow walk ran");

    let after = pool.call("head", rotate, echo).await;
    assert!(
        matches!(after, Err(NetError::NoneReady { .. })),
        "an endpoint disqualified for serving another network should never be asked \
         again this session, but the next walk was answered by it: {after:?}"
    );
}

#[test]
fn every_transport_has_a_budget_and_monero_gets_the_slow_one() {
    for transport in Transport::ALL {
        let policy = TimeoutPolicy::for_transport(transport);
        assert!(policy.connect <= policy.per_attempt, "{transport}");
        assert!(policy.per_attempt < policy.walk, "{transport}");
        assert!(policy.floor < policy.per_attempt, "{transport}");
    }
    let slow = TimeoutPolicy::for_transport(Transport::MoneroRpc);
    assert!(slow.per_attempt > TimeoutPolicy::for_transport(Transport::HttpJsonrpc).per_attempt);
}

#[tokio::test(start_paused = true)]
async fn a_call_at_one_endpoint_reaches_that_endpoint_and_no_other() {
    let harness =
        harness(&[ENDPOINT_A, ENDPOINT_B, ENDPOINT_C]).expect("a pool over three endpoints");

    let answered = harness
        .pool
        .call_at(ENDPOINT_B, "follow up", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect("the named endpoint answers");
    assert_eq!(answered, ENDPOINT_B);
    assert_eq!(*harness.connector.connects.lock(), vec![ENDPOINT_B.to_owned()]);

    harness.connector.refused_urls.lock().push(ENDPOINT_C.to_owned());
    let error = harness
        .pool
        .call_at(ENDPOINT_C, "follow up", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("the named endpoint refused the dial");
    assert!(error.is_unreachable(), "{error}");
    assert_eq!(
        *harness.connector.connects.lock(),
        vec![ENDPOINT_B.to_owned(), ENDPOINT_C.to_owned()],
        "a failure there is not rotated to another endpoint"
    );
    assert_eq!(harness.health.entry(harness.pool.chain(), ENDPOINT_C).health.failures, 1);
}

#[tokio::test(start_paused = true)]
async fn a_call_at_an_endpoint_that_answered_badly_is_not_unreachable() {
    let harness = harness(&[ENDPOINT_A]).expect("a pool over one endpoint");
    let error = harness
        .pool
        .call_at(ENDPOINT_A, "follow up", rotate, async |_connection| Err::<(), _>(refused()))
        .await
        .expect_err("the endpoint refused");
    assert!(!error.is_unreachable(), "it answered: {error}");
}

#[tokio::test(start_paused = true)]
async fn a_call_at_a_removed_or_disqualified_endpoint_is_unreachable() {
    let harness = harness(&[ENDPOINT_A, ENDPOINT_B]).expect("a pool over two endpoints");
    let error = harness
        .pool
        .call_at(ENDPOINT_C, "follow up", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("that endpoint is not configured");
    assert!(matches!(error, NetError::NoEndpoints { .. }), "{error}");
    assert!(error.is_unreachable());
    assert!(harness.connector.connects.lock().is_empty(), "nothing was dialled");

    harness.connector.wrong_network_urls.lock().push(ENDPOINT_A.to_owned());
    let _ = harness.pool.call("head", rotate, async |connection| Ok(connection.url.clone())).await;
    let error = harness
        .pool
        .call_at(ENDPOINT_A, "follow up", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("a serves another network");
    assert!(matches!(error, NetError::WrongNetwork { .. }), "{error}");
    assert!(error.is_unreachable());
}
