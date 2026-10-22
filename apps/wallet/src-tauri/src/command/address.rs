use jelly_core::{ChainId, CoreError, DerivationPath, ScriptKind, SecretText};
use jelly_engine::AddressRecord;
use secrecy::SecretString;
use tauri::State;

use crate::command::{open_explorer, with_engine};
use crate::error::IpcError;
use crate::state::AppState;

const ADDRESS_PLACEHOLDER: &str = "{address}";

#[tauri::command]
#[specta::specta]
pub(crate) async fn list_addresses(
    state: State<'_, AppState>,
    chain: ChainId,
) -> Result<Vec<AddressRecord>, IpcError> {
    with_engine(&state, async move |engine| engine.list_addresses(Some(chain)).await).await
}

/// Opens only a built-in address explorer after validating the address.
#[tauri::command]
#[specta::specta]
pub(crate) async fn open_address_explorer(chain: ChainId, address: String) -> Result<(), IpcError> {
    let chains = jelly_chains::chains();
    let template = chains
        .spec(chain)
        .explorer_address_url
        .ok_or_else(|| IpcError::invalid_input("This chain does not have an address explorer."))?;
    let address = chains.validate_address(chain, &address).map_err(CoreError::from)?;
    open_explorer(template.replace(ADDRESS_PLACEHOLDER, address.value())).await
}

/// Derives the lowest free address using the requested script type or the chain's
/// default path.
#[tauri::command]
#[specta::specta]
pub(crate) async fn add_address(
    state: State<'_, AppState>,
    chain: ChainId,
    script: Option<ScriptKind>,
    label: Option<String>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.add_address(chain, script, label).await.map(|_| ())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn add_address_at(
    state: State<'_, AppState>,
    chain: ChainId,
    path: DerivationPath,
    script: Option<ScriptKind>,
    label: Option<String>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.add_address_at(chain, &path, script, label).await.map(|_| ())
    })
    .await
}

/// Creates a standalone key in the vault. The seed phrase cannot restore it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn create_imported_address(
    state: State<'_, AppState>,
    chain: ChainId,
    script: Option<ScriptKind>,
    label: Option<String>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.create_imported_address(chain, script, label).await.map(|_| ())
    })
    .await
}

/// The key password is the keystore-v3 password and is ignored by every other import
/// format.
#[tauri::command]
#[specta::specta]
pub(crate) async fn import_private_key(
    state: State<'_, AppState>,
    chain: ChainId,
    input: String,
    key_password: Option<String>,
    script: Option<ScriptKind>,
    label: Option<String>,
) -> Result<(), IpcError> {
    let input = SecretText::new(input);
    let key_password = key_password.map(SecretString::from);
    with_engine(&state, async move |engine| {
        engine.import_private_key(chain, input, key_password, script, label).await.map(|_| ())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_address_label(
    state: State<'_, AppState>,
    address_id: i64,
    label: Option<String>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.set_address_label(address_id, label).await.map(|_| ())
    })
    .await
}

/// Removes an address the seed derives, with its balances and history. Adding it again
/// re-derives it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_address(
    state: State<'_, AppState>,
    address_id: i64,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.delete_address(address_id).await).await
}

/// Permanently deletes a standalone key and its address.
#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_key(
    state: State<'_, AppState>,
    address_id: i64,
    password: String,
    confirmation: String,
) -> Result<(), IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.delete_key(address_id, password, &confirmation).await
    })
    .await
}

/// Returns the canonical address without accessing keys or the network.
#[tauri::command]
#[specta::specta]
pub(crate) async fn validate_address(chain: ChainId, value: String) -> Result<String, IpcError> {
    let address =
        jelly_chains::chains().validate_address(chain, &value).map_err(CoreError::from)?;
    Ok(address.into_value())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_xpub(
    state: State<'_, AppState>,
    path: DerivationPath,
) -> Result<String, IpcError> {
    with_engine(&state, async move |engine| engine.get_xpub(&path).await).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_xpub_for_address(
    state: State<'_, AppState>,
    address_id: i64,
) -> Result<Option<String>, IpcError> {
    with_engine(&state, async move |engine| engine.get_xpub_for_address(address_id).await).await
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, ErrorCode};

    use super::open_address_explorer;
    use crate::error::IpcError;

    #[tokio::test]
    async fn an_address_that_could_change_the_explorer_destination_is_refused() {
        for chain in ChainId::ALL {
            for address in [
                "",
                "..",
                "../address/other",
                "abc?redirect=elsewhere",
                "abc#other",
                "abc%2fother",
                "abc\\other",
                "abc\" --other",
                "abc & other",
                "https://example.invalid/",
                "file:///C:/example",
                "abc\nother",
                "abc\0other",
                "\u{200b}abc",
            ] {
                let error = open_address_explorer(chain, address.to_owned()).await.unwrap_err();
                assert_eq!(error.code, ErrorCode::InvalidInput.into());
            }
        }
    }

    #[tokio::test]
    async fn a_chain_without_an_address_explorer_is_refused() {
        let error = open_address_explorer(ChainId::Monero, String::new()).await.unwrap_err();
        assert_eq!(error, IpcError::invalid_input("This chain does not have an address explorer."));
    }
}
