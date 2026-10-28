use std::iter::repeat_n;

use jelly_electrum::{
    ElectrumError, Method, ProtocolVersion,
    test_util::{Answer, FakeElectrum, endpoint_label},
};

use crate::common::{
    FEE_HISTOGRAM, LISTUNSPENT, header_hex, histogram_buckets, quoted, raw, script, strings_at, tx,
    vector,
};

fn histogram() -> Answer {
    Answer::Result(raw(FEE_HISTOGRAM, "/result"))
}

fn unspents() -> Answer {
    Answer::Result(raw(LISTUNSPENT, "/result"))
}

mod framing {
    use super::*;

    #[tokio::test]
    async fn a_line_that_arrives_in_pieces_still_decodes() {
        let fake = FakeElectrum::new();
        // Eight bytes at a time in both directions, so no read ever sees a whole
        // line.
        fake.set_pipe_capacity(8);
        fake.always(Method::FeeHistogram, histogram());
        let client = fake.client().await.expect("the fake negotiates");
        assert_eq!(client.fee_histogram().await.expect("a histogram").len(), histogram_buckets());
    }

    #[tokio::test]
    async fn replies_that_come_back_reversed_reach_the_right_caller() {
        let fake = FakeElectrum::new();
        fake.always(Method::ListUnspent, Answer::Deferred(raw(LISTUNSPENT, "/result")));
        fake.always(Method::FeeHistogram, histogram());
        let client = fake.client().await.expect("the fake negotiates");

        let scripts = [script()];
        let (listed, buckets) = tokio::join!(client.unspents(&scripts), client.fee_histogram());

        let listed = listed.expect("the deferred answer still lands");
        let outpoints: Vec<String> = listed
            .first()
            .expect("one script, one list")
            .iter()
            .map(|unspent| unspent.outpoint.to_string())
            .collect();
        assert_eq!(outpoints, strings_at(&vector(LISTUNSPENT), "/expect/outpoints"));
        assert_eq!(buckets.expect("a histogram").len(), histogram_buckets());
    }

    #[tokio::test]
    async fn a_line_that_is_not_json_rpc_ends_the_socket() {
        let fake = FakeElectrum::new();
        fake.on(Method::FeeHistogram, Answer::Result("not json".to_owned()));
        fake.always(Method::FeeHistogram, histogram());
        let client = fake.client().await.expect("the fake negotiates");

        let error = client.fee_histogram().await.expect_err("the line is refused");
        assert!(
            matches!(error, ElectrumError::Protocol { method: Method::FeeHistogram, .. }),
            "{error}"
        );

        // The socket is gone, so the next call opens another one rather than
        // failing forever.
        assert_eq!(client.fee_histogram().await.expect("a histogram").len(), histogram_buckets());
        assert_eq!(fake.sockets_opened(), 2);
    }

    #[tokio::test]
    async fn an_answer_that_will_not_decode_keeps_the_socket() {
        let fake = FakeElectrum::new();
        fake.on(Method::FeeHistogram, Answer::Result("null".to_owned()));
        fake.always(Method::FeeHistogram, histogram());
        let client = fake.client().await.expect("the fake negotiates");

        let error = client.fee_histogram().await.expect_err("null is not a histogram");
        assert!(matches!(error, ElectrumError::Protocol { .. }), "{error}");
        assert_eq!(client.fee_histogram().await.expect("a histogram").len(), histogram_buckets());
        assert_eq!(fake.sockets_opened(), 1, "one undecodable answer is not a dead socket");
    }
}

mod lifecycle {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_request_that_times_out_retires_the_socket() {
        let fake = FakeElectrum::new();
        fake.on(Method::FeeHistogram, Answer::Silence);
        fake.always(Method::FeeHistogram, histogram());
        let client = fake.client().await.expect("the fake negotiates");

        let error = client.fee_histogram().await.expect_err("the request times out");
        assert!(matches!(error, ElectrumError::Timeout { .. }), "{error}");
        // The timeout retired the silent socket, so the next call dials a fresh one.
        assert_eq!(client.fee_histogram().await.expect("a histogram").len(), histogram_buckets());
        assert_eq!(fake.sockets_opened(), 2);
    }

    #[tokio::test]
    async fn a_server_hanging_up_is_retried_once_on_a_fresh_socket() {
        let fake = FakeElectrum::new();
        fake.on(Method::FeeHistogram, Answer::HangUp);
        fake.always(Method::FeeHistogram, histogram());
        let client = fake.client().await.expect("the fake negotiates");

        assert_eq!(client.fee_histogram().await.expect("a histogram").len(), histogram_buckets());
        assert_eq!(fake.sockets_opened(), 2, "one dial, one re-dial");
    }

    #[tokio::test]
    async fn a_server_that_keeps_hanging_up_is_not_retried_forever() {
        let fake = FakeElectrum::new();
        fake.always(Method::FeeHistogram, Answer::HangUp);
        let client = fake.client().await.expect("the fake negotiates");

        let error = client.fee_histogram().await.expect_err("the socket keeps closing");
        assert!(matches!(error, ElectrumError::Closed { .. }), "{error}");
        assert_eq!(fake.sockets_opened(), 2, "exactly one retry");
    }

    #[tokio::test]
    async fn a_broadcast_is_never_retried() {
        let fake = FakeElectrum::new();
        fake.always(Method::Broadcast, Answer::HangUp);
        let client = fake.client().await.expect("the fake negotiates");

        let error = client.broadcast(&tx()).await.expect_err("the socket closes");
        assert!(matches!(error, ElectrumError::Closed { .. }), "{error}");
        assert_eq!(fake.sockets_opened(), 1, "a send that may have landed is never repeated");
    }

    #[tokio::test]
    async fn the_negotiated_version_is_what_the_server_offered() {
        let fake = FakeElectrum::new();
        let client = fake.client().await.expect("the fake negotiates");
        assert_eq!(client.protocol().await, Some(ProtocolVersion::V1_6));
        assert_eq!(client.software().await.as_deref(), Some("Fulcrum 1.12.0"));
        assert_eq!(client.endpoint(), &endpoint_label());
    }

    #[tokio::test]
    async fn the_handshake_is_first_and_names_the_client_and_the_range() {
        let fake = FakeElectrum::new();
        let client = fake.client().await.expect("the fake negotiates");
        drop(client);
        let calls = fake.calls();
        let first = calls.first().expect("the handshake is the first call");
        assert_eq!(first.method, Method::ServerVersion);
        assert!(first.params.contains("\"jelly\""), "{}", first.params);
        assert!(first.params.contains("\"1.4\""), "{}", first.params);
        assert!(first.params.contains("\"1.6\""), "{}", first.params);
    }
}

mod batching {
    use super::*;

    #[tokio::test]
    async fn an_empty_request_never_reaches_the_wire() {
        let fake = FakeElectrum::new();
        let client = fake.client().await.expect("the fake negotiates");
        client.verify_identity().await.expect("the socket is proved");
        fake.forget_calls();
        assert!(client.histories(&[]).await.expect("nothing to ask").is_empty());
        assert!(client.block_headers(&[]).await.expect("nothing to ask").is_empty());
        assert!(client.txs(&[]).await.expect("nothing to ask").is_empty());
        assert!(client.unspents(&[]).await.expect("nothing to ask").is_empty());
        assert!(fake.calls().is_empty(), "empty requests need no further calls");
        assert_eq!(fake.sockets_opened(), 1);
    }

    #[tokio::test]
    async fn a_batch_answered_with_fewer_elements_is_refused() {
        let fake = FakeElectrum::new();
        fake.always(Method::ListUnspent, Answer::Each(vec![raw(LISTUNSPENT, "/result")]));
        let client = fake.client().await.expect("the fake negotiates");
        let error =
            client.unspents(&[script(), script()]).await.expect_err("a short batch is refused");
        assert!(
            matches!(error, ElectrumError::BatchLength { requested: 2, received: 1, .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_batch_answered_with_errors_fails_with_the_server_error() {
        let fake = FakeElectrum::new();
        fake.always(Method::ListUnspent, Answer::Error { code: -32600, message: "no".to_owned() });
        let client = fake.client().await.expect("the fake negotiates");
        let error = client.unspents(&[script(), script()]).await.expect_err("the batch fails");
        assert!(matches!(error, ElectrumError::Rpc { code: -32600, .. }), "{error}");
    }

    /// `electrum.blockstream.info`, a shipped Bitcoin endpoint running Blockstream's
    /// electrs, drops the socket on a batch of more than 20.
    ///
    /// The fake answers at most 20 per batch, and a sync pass asks for every watched
    /// script's history in one call.
    #[tokio::test]
    async fn a_large_request_fits_the_batch_a_shipped_server_accepts() {
        let fake = FakeElectrum::new();
        fake.always(Method::GetHistory, Answer::Each(repeat_n("[]".to_owned(), 20).collect()));
        let client = fake.client().await.expect("the fake negotiates");
        let scripts = vec![script(); 40];
        let histories = client
            .histories(&scripts)
            .await
            .unwrap_or_else(|error| panic!("forty scripts: {error}"));
        assert_eq!(histories.len(), 40);
    }

    #[tokio::test]
    async fn a_request_larger_than_the_batch_limit_is_split() {
        let fake = FakeElectrum::new();
        let client = fake.client().await.expect("the fake negotiates");
        client.verify_identity().await.expect("the socket is proved");
        fake.forget_calls();
        // Twenty results per answer, so a single unsplit batch of forty-five
        // would come back short and be refused.
        fake.always(Method::BlockHeader, Answer::Each(vec![quoted(&header_hex(0)); 20]));

        // Non-consecutive heights, so the linkage check has nothing to say.
        let heights: Vec<u32> = (0..90).step_by(2).collect();
        let headers = client.block_headers(&heights).await.expect("forty-five headers");
        assert_eq!(headers.len(), 45);
        assert_eq!(fake.calls_to(Method::BlockHeader).len(), 45);
    }
}

mod pushes {
    use super::*;

    #[tokio::test]
    async fn an_unsolicited_push_is_dropped_and_the_socket_carries_on() {
        let fake = FakeElectrum::new();
        fake.always(Method::FeeHistogram, histogram());
        fake.always(Method::ListUnspent, unspents());
        let client = fake.client().await.expect("the fake negotiates");

        fake.push_notification(
            Method::HeadersSubscribe,
            &format!(r#"[{{"height":1,"hex":"{}"}}]"#, header_hex(1)),
        );
        fake.push_notification(Method::HeadersSubscribe, "[42]");
        assert_eq!(client.fee_histogram().await.expect("a histogram").len(), histogram_buckets());

        assert_eq!(client.unspents(&[script()]).await.expect("unspents").len(), 1);
        assert_eq!(fake.sockets_opened(), 1, "a push is never worth a working socket");
    }
}
