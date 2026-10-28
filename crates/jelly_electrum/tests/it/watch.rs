use std::sync::Arc;
use std::time::Duration;

use jelly_electrum::{
    ElectrumError, Method,
    test_util::{Answer, FakeElectrum, bitcoin_genesis_header},
};
use tokio::time::timeout;

use crate::common::script;

/// A `blockchain.headers.subscribe` answer built from the genesis header, the one
/// header the fake always has.
fn tip_answer() -> Answer {
    Answer::Result(format!(r#"{{"height":100,"hex":"{}"}}"#, bitcoin_genesis_header()))
}

fn watching_fake() -> FakeElectrum {
    let fake = FakeElectrum::new();
    fake.always(Method::HeadersSubscribe, tip_answer());
    fake.always(Method::ScripthashSubscribe, Answer::Result("null".to_owned()));
    fake
}

#[tokio::test]
async fn a_fresh_registration_resolves_immediately_and_registers_both_subscriptions() {
    let fake = watching_fake();
    let client = fake.client().await.expect("the fake negotiates");
    client.watch(&[script()]).await.expect("a fresh registration resolves");
    assert!(fake.has_been_called(Method::HeadersSubscribe), "the tip registration went out");
    assert_eq!(fake.calls_to(Method::ScripthashSubscribe).len(), 1, "one script registration");
}

#[tokio::test(start_paused = true)]
async fn a_server_push_wakes_a_waiting_watch() {
    let fake = watching_fake();
    let client = Arc::new(fake.client().await.expect("the fake negotiates"));
    client.watch(&[]).await.expect("the registration pass");

    let waiter = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.watch(&[]).await }
    });

    // The fake sends queued pushes with its next answer, so any request flushes this
    // one. The counter keeps the wake race-free.
    fake.push_notification(Method::HeadersSubscribe, r#"[{"height":101}]"#);
    client.tip_height().await.expect("a request that flushes the push");

    timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the push wakes the watch")
        .expect("the waiter task runs")
        .expect("the woken watch is an activity hint");
}

#[tokio::test(start_paused = true)]
async fn a_changed_script_set_reregisters_before_waiting() {
    let fake = watching_fake();
    let client = fake.client().await.expect("the fake negotiates");
    client.watch(&[]).await.expect("the registration pass");
    assert!(
        fake.calls_to(Method::ScripthashSubscribe).is_empty(),
        "an empty set registers nothing"
    );

    // The push arrives with the re-registration's own answer, so the call both
    // registers the new script and wakes on it. The counter keeps the wake
    // race-free whichever line the reader handles first.
    fake.push_notification(Method::ScripthashSubscribe, r#"["ab","cd"]"#);
    timeout(Duration::from_secs(5), client.watch(&[script()]))
        .await
        .expect("the re-registration is woken by the push")
        .expect("the woken watch is an activity hint");
    assert_eq!(fake.calls_to(Method::ScripthashSubscribe).len(), 1, "the new set was registered");
}

#[tokio::test]
async fn a_refused_script_registration_degrades_to_headers_instead_of_failing() {
    let fake = watching_fake();
    fake.always(
        Method::ScripthashSubscribe,
        Answer::Error { code: 1, message: "history too large".to_owned() },
    );
    let client = fake.client().await.expect("the fake negotiates");
    client.watch(&[script()]).await.expect("a refusal costs granularity, not the watch");
    assert!(fake.has_been_called(Method::HeadersSubscribe), "the header registration stands");
}

#[tokio::test(start_paused = true)]
async fn a_dead_socket_fails_the_watch_and_the_next_call_reregisters() {
    let fake = watching_fake();
    let client = Arc::new(fake.client().await.expect("the fake negotiates"));
    client.watch(&[]).await.expect("the registration pass");
    let sockets_before = fake.sockets_opened();

    let waiter = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.watch(&[]).await }
    });

    // Killing the socket must wake the waiter with an error, not strand it.
    fake.on(Method::HeadersSubscribe, Answer::HangUp);
    let _ = client.tip_height().await;
    let outcome = timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the close wakes the watch")
        .expect("the waiter task runs");
    assert!(matches!(outcome, Err(ElectrumError::Closed { .. })), "{outcome:?}");

    // The next call re-dials underneath and registers afresh.
    let registrations_before = fake.calls_to(Method::HeadersSubscribe).len();
    client.watch(&[]).await.expect("the re-registration resolves");
    assert!(fake.sockets_opened() > sockets_before, "a fresh socket was dialed");
    assert!(
        fake.calls_to(Method::HeadersSubscribe).len() > registrations_before,
        "the tip was registered again"
    );
}
