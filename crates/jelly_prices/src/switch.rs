use tokio::sync::watch;

/// True once `enabled` holds `true`, or false once its sender is dropped.
pub(crate) async fn wait_until_enabled(enabled: &mut watch::Receiver<bool>) -> bool {
    loop {
        if *enabled.borrow_and_update() {
            return true;
        }
        if enabled.changed().await.is_err() {
            return false;
        }
    }
}

/// True once `enabled` changes to `false`, or false once its sender is dropped.
/// A repeated `true` is ignored.
pub(crate) async fn wait_until_disabled(enabled: &mut watch::Receiver<bool>) -> bool {
    loop {
        if enabled.changed().await.is_err() {
            return false;
        }
        if !*enabled.borrow_and_update() {
            return true;
        }
    }
}
