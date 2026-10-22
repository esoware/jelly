use std::time::Duration;

use jelly_core::ChainAdapter as _;
use jelly_electrum::Method;
use tokio::time::timeout;

use crate::common::{CHAINS, Setup, kind_of, synced};

const FUNDING_SAT: u64 = 190_000;

#[tokio::test(start_paused = true)]
async fn a_server_push_resolves_the_activity_listener() {
    for params in CHAINS {
        let (harness, _wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        harness.serve(Method::ScripthashSubscribe, "null");

        // The first listen registers afresh and resolves at once, because anything
        // before the registration is invisible and a pass has to cover the gap.
        timeout(Duration::from_secs(5), harness.adapter.wait_activity()).await.unwrap();
        assert!(
            harness.chain.server.has_been_called(Method::ScripthashSubscribe),
            "the scripts registered"
        );

        // The second listen waits for a real push. The fake delivers queued pushes
        // with the next answer, so a concurrent pass flushes it.
        let woken = harness.adapter.wait_activity();
        let trigger = async {
            harness.chain.server.push_notification(Method::HeadersSubscribe, r#"[{"height":101}]"#);
            harness.sync().await.expect("the pass flushes the push");
        };
        timeout(Duration::from_secs(5), async {
            tokio::join!(woken, trigger);
        })
        .await
        .unwrap();
    }
}
