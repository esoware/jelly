use jelly_core::{ChainAdapter as _, CursorBytes, CursorScope, CursorStore as _};
use jelly_utxo::test_util::claimed;

use crate::common::{CHAINS, Setup, kind_of, synced, watched};

const FUNDING_SAT: u64 = 500_000;
const PAYMENT_SAT: u64 = 100_000;

#[tokio::test]
async fn a_build_claims_its_coins_and_a_discard_releases_them() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let send = harness.request(wallet.theirs.address(), PAYMENT_SAT, "2");

        let unsigned = harness.adapter.build_tx(&send).await.unwrap();
        assert_eq!(claimed(&harness.adapter).len(), 1);
        assert!(
            harness.adapter.build_tx(&send).await.is_err(),
            "a coin committed to a pending transaction is not spendable twice"
        );

        let tracker = harness.adapter.pending_tracker().expect("the adapter tracks pending sends");
        tracker.discard(&unsigned).await.unwrap();
        assert!(claimed(&harness.adapter).is_empty());
        let again = harness.adapter.build_tx(&send).await.unwrap();
        assert_eq!(again.payload, unsigned.payload, "the same request builds the same transaction");
    }
}

#[tokio::test]
async fn a_signed_send_keeps_its_claim_until_it_is_discarded() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let unsigned = harness
            .adapter
            .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "2"))
            .await
            .unwrap();
        let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();

        let tracker = harness.adapter.pending_tracker().expect("the adapter tracks pending sends");
        assert_eq!(claimed(&harness.adapter).len(), 1, "the build claimed them");
        assert_eq!(signed.chain(), params.chain);

        tracker.discard(&unsigned).await.unwrap();
        assert!(claimed(&harness.adapter).is_empty(), "a discard is the only release");
    }
}

#[tokio::test]
async fn an_unreadable_cursor_blocks_every_operation_and_is_never_overwritten() {
    for params in CHAINS {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let send = harness.request(wallet.theirs.address(), PAYMENT_SAT, "2");
        let unsigned = harness.adapter.build_tx(&send).await.unwrap();
        let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
        harness
            .cursors
            .advance(
                CursorScope::Address(7),
                params.scan_stream,
                CursorBytes::copied(b"unreadable conflict state"),
            )
            .await
            .unwrap();
        let writes = harness.cursors.advance_count();
        harness.watch(&[watched(&params, 7, wallet.ours.address())]).await.unwrap();
        harness.sync().await.unwrap_err();
        for _ in 0..2 {
            assert!(harness.watch(&[watched(&params, 7, wallet.ours.address())]).await.is_err());
            harness.adapter.build_tx(&send).await.unwrap_err();
            harness.adapter.broadcast(&signed).await.unwrap_err();
            harness.sync().await.unwrap_err();
        }
        assert_eq!(harness.cursors.advance_count(), writes);
        assert!(
            harness
                .cursors
                .load_scan(CursorScope::Address(7), params.scan_stream)
                .await
                .unwrap()
                .unwrap()
                .with_bytes(|bytes| bytes == b"unreadable conflict state")
        );
    }
}
