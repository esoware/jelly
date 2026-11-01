use tokio::sync::watch;

/// True once `enabled` holds `true`, or false once its sender is dropped.
pub(crate) async fn wait_until_enabled(enabled: &mut watch::Receiver<bool>) -> bool {
    enabled.wait_for(|&is_enabled| is_enabled).await.is_ok()
}

/// True once `enabled` holds `false`, or false once its sender is dropped. Every caller
/// has last seen `true`, so this is the switch turning off.
pub(crate) async fn wait_until_disabled(enabled: &mut watch::Receiver<bool>) -> bool {
    enabled.wait_for(|&is_enabled| !is_enabled).await.is_ok()
}
