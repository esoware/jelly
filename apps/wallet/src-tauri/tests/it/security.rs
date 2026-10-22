//! Checks the shipped capability and network restrictions, not test copies.

use std::collections::BTreeSet;
use std::fs;

use jelly_lib::test_util::COMMAND_NAMES;
use serde_json::{Value, json};

use crate::common::{Fallible, manifest_dir, tauri_conf};

/// Secret exports and capture protection require a separate capability grant.
const SECRET_COMMANDS: [&str; 6] = [
    "export_seed_phrase",
    "export_seed_passphrase",
    "export_seed_entropy_hex",
    "export_account_seed",
    "export_private_key",
    "set_secret_screen",
];

/// Window chrome and event listening. Anything else from `core:` needs a reason and a
/// line here, and a plugin permission needs both in `PLUGIN_PERMISSIONS`.
const CORE_PERMISSIONS: [&str; 9] = [
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    "core:window:allow-set-title",
    "core:window:allow-minimize",
    "core:window:allow-toggle-maximize",
    "core:window:allow-internal-toggle-maximize",
    "core:window:allow-close",
    "core:window:allow-start-dragging",
    "core:window:allow-is-maximized",
];

/// Native text writes let non-secret copies reach clipboard history without leaving incognito
/// mode.
const PLUGIN_PERMISSIONS: [&str; 1] = ["clipboard-manager:allow-write-text"];

const MAIN_CAPABILITY: &str = "main.json";
const SECRET_CAPABILITY: &str = "secret-exports.json";

const CONNECT_SRC: &str = "connect-src ipc: http://ipc.localhost";

fn capability(file: &str) -> Fallible<Value> {
    let text = fs::read_to_string(manifest_dir().join("capabilities").join(file))?;
    Ok(serde_json::from_str(&text)?)
}

/// A scoped permission is an object, and its identifier is what it grants.
fn permissions(file: &str) -> Fallible<Vec<String>> {
    let capability = capability(file)?;
    let entries =
        capability.get("permissions").and_then(Value::as_array).ok_or("no permissions list")?;
    entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("identifier").and_then(Value::as_str))
                .map(str::to_owned)
                .ok_or_else(|| "a permission with no identifier".into())
        })
        .collect()
}

fn granted(file: &str) -> Fallible<BTreeSet<String>> {
    Ok(permissions(file)?
        .iter()
        .filter_map(|permission| permission.strip_prefix("allow-"))
        .map(|command| command.replace('-', "_"))
        .collect())
}

#[test]
fn the_capabilities_grant_exactly_the_mounted_commands() {
    let mounted: BTreeSet<String> = COMMAND_NAMES.iter().map(|name| (*name).to_owned()).collect();
    let main_commands = granted(MAIN_CAPABILITY).unwrap();
    let secrets = granted(SECRET_CAPABILITY).unwrap();

    let overlapping: Vec<&String> = main_commands.intersection(&secrets).collect();
    assert!(overlapping.is_empty(), "a command is granted by both capabilities: {overlapping:?}");

    let all: BTreeSet<String> = main_commands.union(&secrets).cloned().collect();
    let ungranted: Vec<&String> = mounted.difference(&all).collect();
    let unmounted: Vec<&String> = all.difference(&mounted).collect();
    assert!(ungranted.is_empty(), "mounted but no window may call it: {ungranted:?}");
    assert!(unmounted.is_empty(), "granted but not mounted: {unmounted:?}");
}

#[test]
fn only_the_secret_commands_are_granted_by_the_secret_capability() {
    let expected: BTreeSet<String> =
        SECRET_COMMANDS.iter().map(|command| (*command).to_owned()).collect();
    assert_eq!(granted(SECRET_CAPABILITY).unwrap(), expected);
}

/// Export naming lets this test catch new secret commands granted by the ordinary
/// capability file.
#[test]
fn no_command_outside_the_secret_capability_is_named_like_an_export() {
    let stray: Vec<&&str> = COMMAND_NAMES
        .iter()
        .filter(|name| name.starts_with("export_") && !SECRET_COMMANDS.contains(*name))
        .collect();
    assert!(stray.is_empty(), "an export command is not on the secret capability: {stray:?}");
}

#[test]
fn the_page_can_neither_forge_events_nor_take_the_core_defaults() {
    let main = permissions(MAIN_CAPABILITY).unwrap();
    assert!(
        !main.iter().any(|permission| permission == "core:default"),
        "core:default grants far more than this app needs"
    );
    assert!(
        !main.iter().any(|permission| permission == "core:event:allow-emit"),
        "an injected script could forge a lock-state event"
    );
}

/// A plugin permission such as `http:allow-fetch` or `opener:allow-open-url` would let the
/// page reach the network through Rust, and a webview the page creates would carry none
/// of the navigation guards.
#[test]
fn the_capabilities_grant_only_commands_and_the_listed_core_and_plugin_permissions() {
    for file in [MAIN_CAPABILITY, SECRET_CAPABILITY] {
        let permissions = permissions(file).unwrap();
        assert!(!permissions.is_empty(), "{file} grants nothing, so the parser is wrong");
        let stray: Vec<String> = permissions
            .into_iter()
            .filter(|permission| {
                !permission.starts_with("allow-")
                    && !CORE_PERMISSIONS.contains(&permission.as_str())
                    && !PLUGIN_PERMISSIONS.contains(&permission.as_str())
            })
            .collect();
        assert!(stray.is_empty(), "{file} grants a permission outside the policy: {stray:?}");
    }
}

#[test]
fn native_clipboard_access_is_write_only() {
    let is_clipboard = |permission: &String| permission.starts_with("clipboard-manager:");
    let clipboard_permissions: Vec<String> =
        permissions(MAIN_CAPABILITY).unwrap().into_iter().filter(is_clipboard).collect();
    assert_eq!(clipboard_permissions, ["clipboard-manager:allow-write-text"]);
    assert!(!permissions(SECRET_CAPABILITY).unwrap().iter().any(is_clipboard));
}

#[test]
fn sensitive_browser_copies_keep_incognito_clipboard_protection() {
    let config = tauri_conf().unwrap();
    assert_eq!(config.pointer("/app/windows/0/incognito"), Some(&Value::Bool(true)));
}

/// Scope both capability files to the wallet window so future windows do not inherit them.
#[test]
fn every_capability_is_scoped_to_the_wallet_window() {
    for file in [MAIN_CAPABILITY, SECRET_CAPABILITY] {
        let capability = capability(file).unwrap();
        assert_eq!(
            capability.get("windows"),
            Some(&json!(["main"])),
            "{file} is not scoped to one window"
        );
    }
}

/// Blocking webview networking prevents injected scripts from sending secrets elsewhere.
#[test]
fn the_page_has_no_network_egress() {
    let config = tauri_conf().unwrap();
    let csp = config
        .pointer("/app/security/csp")
        .and_then(Value::as_str)
        .expect("the content security policy is a string");
    let directives: Vec<&str> = csp.split(';').map(str::trim).collect();
    let connect_directives: Vec<&str> = directives
        .iter()
        .copied()
        .filter(|directive| directive.starts_with("connect-src"))
        .collect();
    assert_eq!(
        connect_directives,
        [CONNECT_SRC],
        "the content security policy must be exactly {CONNECT_SRC}"
    );
    for directive in ["default-src 'self'", "object-src 'none'", "frame-src 'none'"] {
        assert!(directives.contains(&directive), "the content security policy dropped {directive}");
    }
    assert_ne!(
        config.pointer("/app/security/dangerousDisableAssetCspModification"),
        Some(&Value::Bool(true)),
        "the asset protocol must not be allowed to rewrite the policy"
    );
}

/// The policy cannot stop a top-level navigation, so the window is built in code with a
/// handler that can. A window Tauri creates from the configuration would have none.
#[test]
fn the_wallet_window_is_built_in_code() {
    let config = tauri_conf().unwrap();
    assert_eq!(
        config.pointer("/app/windows/0/create"),
        Some(&Value::Bool(false)),
        "tauri must leave window creation to window.rs"
    );
}

/// A preconnect or dns-prefetch link reaches the network without any policy directive
/// covering it.
#[test]
fn the_page_references_no_remote_resource() {
    let index = fs::read_to_string(manifest_dir().join("../index.html")).unwrap();
    for marker in ["http:", "https:", "//"] {
        assert!(!index.contains(marker), "index.html references a remote resource: {marker}");
    }
}

/// The page gets no global bridge for an injected script to find, and no asset protocol
/// that serves files off disk.
#[test]
fn the_application_ships_one_window_with_no_global_bridge_or_asset_protocol() {
    let config = tauri_conf().unwrap();
    let windows = config.pointer("/app/windows").and_then(Value::as_array);
    assert_eq!(windows.map(Vec::len), Some(1), "more than one window is configured");
    assert_eq!(
        config.pointer("/app/withGlobalTauri"),
        Some(&Value::Bool(false)),
        "the page must not get the global bridge"
    );
    assert_eq!(
        config.pointer("/app/security/assetProtocol/enable"),
        Some(&Value::Bool(false)),
        "the asset protocol must stay off"
    );
}
