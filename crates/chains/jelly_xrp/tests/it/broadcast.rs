use std::sync::Arc;
use std::sync::atomic::Ordering;

use jelly_core::{
    ChainAdapter as _, Deadline,
    test_util::{FakeEventSink, RefusingCursorStore},
};

use super::common::{Node, key, request, watched};

#[tokio::test]
async fn a_send_whose_record_could_not_be_written_never_goes_out() {
    let node = Node::new();
    let cursors = Arc::new(RefusingCursorStore::default());
    cursors.reservation_refusal().store(true, Ordering::SeqCst);
    let adapter = node.adapter_over(Arc::<RefusingCursorStore>::clone(&cursors));
    adapter.set_watch_set(&[watched()]).await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    adapter.broadcast(&signed).await.unwrap_err();
    assert!(node.state.lock().submissions.is_empty());
    adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
    assert!(node.state.lock().submissions.is_empty());
}

#[tokio::test]
async fn a_send_whose_record_could_not_be_written_is_never_sent_later() {
    let node = Node::new();
    let cursors = Arc::new(RefusingCursorStore::default());
    let adapter = node.adapter_over(Arc::<RefusingCursorStore>::clone(&cursors));
    adapter.set_watch_set(&[watched()]).await.unwrap();
    let unsigned = adapter.build_tx(&request()).await.unwrap();
    let signed = adapter.sign_tx(&unsigned, &[key().unwrap()]).unwrap();
    cursors.reservation_refusal().store(true, Ordering::SeqCst);
    let refusal = adapter.broadcast(&signed).await.unwrap_err();
    cursors.reservation_refusal().store(false, Ordering::SeqCst);
    assert!(node.state.lock().submissions.is_empty());
    adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap();
    let submissions = node.state.lock().submissions.len();
    assert_eq!(
        submissions, 0,
        "expected a payment whose broadcast failed with {refusal:?} before reaching the network \
         to stay unsent, but the next sync submitted it {submissions} time(s)"
    );
}
