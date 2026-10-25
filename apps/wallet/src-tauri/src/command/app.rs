use jelly_core::{AssetId, ChainId, CustomFee, FeeLevel, ProxySetting, ScriptKind, wire_enum};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, State};

use crate::error::IpcError;
use crate::state::{AppState, StartupStatus};
use crate::window::{WINDOW_CONTROLS, WindowBackdrop, WindowControls};

/// Asset scale and symbol come from `AssetMeta`.
/// Templates use `{index}`, `{txid}`, and `{address}` placeholders.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate capability the interface reads on its own, not a state of one thing"
)]
pub(crate) struct ChainMeta {
    chain: ChainId,
    name: &'static str,
    fee_rate_unit: Option<&'static str>,
    /// The fee levels a send can pick from, slowest first. Empty on a chain whose
    /// protocol fixes the fee.
    fee_levels: &'static [FeeLevel],
    /// What a custom fee names, or `None` when the chain takes only its levels.
    custom_fee: Option<CustomFee>,
    /// Whether a send can replace the account's next nonce.
    can_pin_nonce: bool,
    /// Whether a send can carry calldata in place of the generated transfer.
    can_carry_data: bool,
    settled_confirmations: u32,
    default_path: &'static str,
    script_kinds: &'static [ScriptKind],
    default_script: Option<ScriptKind>,
    explorer_tx_url: Option<&'static str>,
    explorer_address_url: Option<&'static str>,
    can_bump_fee: bool,
    can_cancel: bool,
    /// Whether a send can delete its sending account, as an XRP account deletion does.
    can_delete_account: bool,
    /// Whether a payment can carry a destination tag, the number a shared receiving
    /// account files it under.
    has_destination_tags: bool,
    /// Whether a payment can carry a comment, the text a recipient credits it by.
    has_comments: bool,
    has_subaddresses: bool,
    can_hold_standalone_keys: bool,
    /// Whether one send can spend the coins of several addresses at once, which only a
    /// UTXO chain can.
    can_pool_sources: bool,
}

/// Asset scale, symbol, and name used by the frontend. Contract addresses remain on
/// `TokenId` for chain adapters.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssetMeta {
    asset: AssetId,
    chain: ChainId,
    symbol: &'static str,
    name: &'static str,
    decimals: u8,
    is_real_world_asset: bool,
}

/// Works before the engine starts, and touches no database, vault, or network.
#[tauri::command]
#[specta::specta]
pub(crate) async fn get_app_version(handle: AppHandle) -> Result<String, IpcError> {
    Ok(handle.package_info().version.to_string())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_data_dir(state: State<'_, AppState>) -> Result<String, IpcError> {
    Ok(state.data_dir().display().to_string())
}

/// Reports startup failures independently of commands that wait for the engine.
#[tauri::command]
#[specta::specta]
pub(crate) async fn get_startup_status(
    state: State<'_, AppState>,
) -> Result<StartupStatus, IpcError> {
    Ok(state.status())
}

/// Saves a replacement proxy without starting networking. Reopen Jelly afterward.
#[tauri::command]
#[specta::specta]
pub(crate) async fn repair_proxy(
    state: State<'_, AppState>,
    proxy: Option<ProxySetting>,
) -> Result<(), IpcError> {
    state.repair_proxy(proxy).await
}

wire_enum! {
    #[derive(Type)]
    pub(crate) enum Platform {
        Macos => "macos",
        Windows => "windows",
        Linux => "linux",
    }
}

const PLATFORM: Platform = cfg_select! {
    target_os = "macos" => Platform::Macos,
    windows => Platform::Windows,
    target_os = "linux" => Platform::Linux,
};

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_platform() -> Result<Platform, IpcError> {
    Ok(PLATFORM)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_window_backdrop(
    backdrop: State<'_, WindowBackdrop>,
) -> Result<WindowBackdrop, IpcError> {
    Ok(*backdrop)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_window_controls() -> Result<WindowControls, IpcError> {
    Ok(WINDOW_CONTROLS)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn list_chain_metadata() -> Result<Vec<ChainMeta>, IpcError> {
    let metadata = jelly_chains::chains().entries().iter().map(|entry| {
        let spec = entry.spec();
        ChainMeta {
            chain: spec.chain,
            name: spec.chain.display_name(),
            fee_rate_unit: spec.fee_rate_unit,
            fee_levels: spec.fee_levels,
            custom_fee: spec.custom_fee,
            can_pin_nonce: spec.can_pin_nonce,
            can_carry_data: spec.can_carry_data,
            settled_confirmations: spec.settled_confirmations,
            default_path: spec.default_path,
            script_kinds: spec.script_kinds,
            default_script: spec.default_script,
            explorer_tx_url: spec.explorer_tx_url,
            explorer_address_url: spec.explorer_address_url,
            can_bump_fee: spec.can_bump_fee,
            can_cancel: spec.can_cancel,
            can_delete_account: spec.can_delete_account,
            has_destination_tags: spec.has_destination_tags,
            has_comments: spec.has_comments,
            has_subaddresses: spec.has_subaddresses,
            can_hold_standalone_keys: spec.can_hold_standalone_keys(),
            can_pool_sources: spec.chain.is_utxo(),
        }
    });
    Ok(metadata.collect())
}

/// Includes the decimals used to display amounts.
#[tauri::command]
#[specta::specta]
pub(crate) async fn list_asset_metadata() -> Result<Vec<AssetMeta>, IpcError> {
    let metadata = AssetId::all().map(|asset| AssetMeta {
        asset,
        chain: asset.chain(),
        symbol: asset.symbol(),
        name: asset.display_name(),
        decimals: asset.decimals(),
        is_real_world_asset: asset.is_real_world_asset(),
    });
    Ok(metadata.collect())
}

#[cfg(test)]
mod tests {
    use jelly_core::{AssetId, ChainId, TokenId};

    use super::{list_asset_metadata, list_chain_metadata};

    #[tokio::test]
    async fn chain_metadata_carries_every_chains_spec_in_chain_order() {
        let all = list_chain_metadata().await.unwrap();
        assert_eq!(all.iter().map(|metadata| metadata.chain).collect::<Vec<_>>(), ChainId::ALL);
        for metadata in &all {
            let chain = metadata.chain;
            let spec = jelly_chains::chains().spec(chain);
            assert_eq!(metadata.name, chain.display_name());
            assert!(!metadata.name.is_empty(), "{}", chain.as_str());
            assert_eq!(metadata.fee_rate_unit, spec.fee_rate_unit, "{}", chain.as_str());
            assert_eq!(metadata.fee_levels, spec.fee_levels, "{}", chain.as_str());
            assert_eq!(metadata.custom_fee, spec.custom_fee, "{}", chain.as_str());
            assert_eq!(metadata.can_pin_nonce, spec.can_pin_nonce, "{}", chain.as_str());
            assert_eq!(metadata.can_carry_data, spec.can_carry_data, "{}", chain.as_str());
            assert_eq!(metadata.settled_confirmations, spec.settled_confirmations);
            assert_eq!(metadata.default_path, spec.default_path);
            assert_eq!(metadata.script_kinds, spec.script_kinds);
            assert_eq!(metadata.default_script, spec.default_script);
            assert_eq!(metadata.explorer_tx_url, spec.explorer_tx_url);
            assert_eq!(metadata.explorer_address_url, spec.explorer_address_url);
            assert_eq!(metadata.can_bump_fee, spec.can_bump_fee, "{}", chain.as_str());
            assert_eq!(metadata.can_cancel, spec.can_cancel, "{}", chain.as_str());
            assert_eq!(metadata.can_delete_account, spec.can_delete_account);
            assert_eq!(metadata.has_destination_tags, spec.has_destination_tags);
            assert_eq!(metadata.has_comments, spec.has_comments);
            assert_eq!(metadata.has_subaddresses, spec.has_subaddresses);
            assert_eq!(metadata.can_hold_standalone_keys, spec.can_hold_standalone_keys());
        }
    }

    #[tokio::test]
    async fn only_a_utxo_chain_pools_its_sources() {
        for metadata in list_chain_metadata().await.unwrap() {
            assert_eq!(metadata.can_pool_sources, metadata.chain.is_utxo(), "{}", metadata.chain);
        }
    }

    #[tokio::test]
    async fn asset_metadata_covers_every_asset() {
        let all = list_asset_metadata().await.unwrap();
        assert_eq!(all.len(), AssetId::COUNT);
        for metadata in &all {
            assert_eq!(metadata.chain, metadata.asset.chain());
            assert_eq!(metadata.decimals, metadata.asset.decimals());
            assert_eq!(metadata.symbol, metadata.asset.symbol());
            assert_eq!(metadata.name, metadata.asset.display_name());
            assert_eq!(metadata.is_real_world_asset, metadata.asset.is_real_world_asset());
            assert!(!metadata.name.is_empty(), "{}", metadata.asset.key());
        }
        let bep = all
            .iter()
            .find(|metadata| metadata.asset == AssetId::Token(TokenId::UsdtBep20))
            .expect("the asset has metadata");
        assert_eq!(bep.decimals, 18);
        assert_eq!(bep.chain, ChainId::Bsc);
    }
}
