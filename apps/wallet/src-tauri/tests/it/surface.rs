use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use jelly_lib::test_util::{COMMAND_NAMES, EVENT_NAMES, export_bindings};
use jelly_prices::Price;
use serde_json::Value;

use crate::common::{Fallible, manifest_dir, tauri_conf};

const BINDINGS_PATH: &str = "../src/ipc/bindings.gen.ts";

const FIAT_PATH: &str = "../src/lib/fiat.ts";

const PACKAGE_PATH: &str = "../package.json";

fn committed_path() -> PathBuf {
    manifest_dir().join(BINDINGS_PATH)
}

fn generated() -> Fallible<String> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("bindings.gen.ts");
    export_bindings(&path)?;
    Ok(fs::read_to_string(path)?)
}

fn mounted_commands(bindings: &str) -> Vec<String> {
    bindings
        .split("__TAURI_INVOKE(\"")
        .skip(1)
        .filter_map(|tail| tail.split('"').next())
        .map(str::to_owned)
        .collect()
}

#[test]
#[ignore = "writes the committed bindings; run with --ignored to refresh"]
fn regenerate_bindings() {
    fs::write(committed_path(), generated().unwrap()).unwrap();
}

#[test]
fn the_committed_bindings_match_the_builder() {
    assert_eq!(
        generated().unwrap(),
        fs::read_to_string(committed_path()).unwrap(),
        "bindings.gen.ts is stale; run `just bindings`"
    );
}

#[test]
fn the_bindings_are_ascii() {
    let offender = generated().unwrap().lines().find(|line| !line.is_ascii()).map(str::to_owned);
    assert!(offender.is_none(), "non-ascii in the generated bindings: {offender:?}");
}

/// IPC integers other than `Amount` and `Price` must stay below 2^53 for lossless conversion
/// to JS numbers.
#[test]
fn no_value_crosses_as_a_bigint() {
    assert!(!generated().unwrap().contains("bigint"), "a bigint reached the bindings");
}

/// Money crosses IPC as decimal strings and is parsed as bigint, never JS number.
#[test]
fn amounts_and_prices_cross_as_decimal_strings() {
    let generated = generated().unwrap();
    for declaration in ["export type Amount = string", "export type Price = string"] {
        assert!(generated.contains(declaration), "missing: {declaration}");
    }
}

/// Skips the runtime code tauri-specta appends.
#[test]
fn every_field_that_crosses_ipc_is_camel_case() {
    const RUNTIME: &str = "/* Tauri Specta runtime */";
    let generated = generated().unwrap();
    let surface = generated.split(RUNTIME).next().unwrap_or(&generated);
    for name in field_names(surface) {
        assert!(!name.contains('_'), "`{name}` crosses IPC in snake case");
        assert!(
            name.starts_with(|c: char| c.is_ascii_lowercase()),
            "`{name}` crosses IPC without a lower case initial"
        );
    }
}

/// Finds declaration identifiers followed by an optional `?` and a colon.
fn field_names(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut names = Vec::new();
    let mut start: Option<usize> = None;
    let mut previous = '\n';
    for (at, &c) in chars.iter().enumerate() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            if start.is_none() && is_field_start(previous) {
                start = Some(at);
            }
            continue;
        }
        if let Some(from) = start.take()
            && is_field_end(&chars, at)
        {
            names.push(chars.get(from..at).map(|word| word.iter().collect()).unwrap_or_default());
        }
        if !c.is_whitespace() || c == '\n' {
            previous = c;
        }
    }
    names
}

const fn is_field_start(previous: char) -> bool {
    matches!(previous, '\n' | '{' | ';' | ',')
}

fn is_field_end(chars: &[char], at: usize) -> bool {
    match chars.get(at) {
        Some(':') => true,
        Some('?') => chars.get(at.saturating_add(1)) == Some(&':'),
        _ => false,
    }
}

/// Prices don't carry their scale over IPC. Rust and TypeScript must agree, or fiat values
/// differ by a power of ten.
#[test]
fn the_interface_reads_a_price_at_the_scale_rust_writes_it() {
    let fiat = fs::read_to_string(manifest_dir().join(FIAT_PATH)).unwrap();
    let declared = format!("const PRICE_DECIMALS = {}", Price::DECIMALS);
    assert!(fiat.contains(&declared), "{FIAT_PATH} does not declare `{declared}`");
}

/// Tauri chooses the working directory of the build hooks, so they name the workspace
/// package rather than a relative path.
#[test]
fn the_interface_is_built_by_name_rather_than_by_path() {
    let package = fs::read_to_string(manifest_dir().join(PACKAGE_PATH)).unwrap();
    let manifest = serde_json::from_str::<Value>(&package).unwrap();
    let name = manifest
        .get("name")
        .and_then(Value::as_str)
        .expect("the interface's manifest declares a package name");

    let config = tauri_conf().unwrap();
    for hook in ["beforeDevCommand", "beforeBuildCommand"] {
        let script = config
            .pointer(&format!("/build/{hook}"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{hook} is not configured"));
        assert!(script.contains(&format!("--filter {name}")), "{hook} is `{script}`");
    }
}

#[test]
fn the_mounted_commands_are_the_declared_ones_in_order() {
    let declared: Vec<String> = COMMAND_NAMES.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(
        mounted_commands(&generated().unwrap()),
        declared,
        "the shared command list and what the bindings invoke have drifted apart"
    );
}

#[test]
fn every_command_name_is_distinct_snake_case() {
    assert!(!COMMAND_NAMES.is_empty(), "no command is mounted");
    for name in COMMAND_NAMES {
        let is_plain =
            name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        assert!(is_plain, "{name} is not a snake_case command name");
    }
    let distinct: BTreeSet<&&str> = COMMAND_NAMES.iter().collect();
    assert_eq!(distinct.len(), COMMAND_NAMES.len(), "a command is mounted twice");
}

#[test]
fn every_event_has_its_own_kebab_case_wire_name() {
    let bindings = generated().unwrap();
    for name in EVENT_NAMES {
        let is_plain = name.chars().all(|c| c.is_ascii_lowercase() || c == '-');
        assert!(is_plain, "{name} is not a kebab-case event name");
        assert!(bindings.contains(name), "{name} is not in the generated bindings");
    }
    let distinct: BTreeSet<&&str> = EVENT_NAMES.iter().collect();
    assert_eq!(distinct.len(), EVENT_NAMES.len(), "two events share a wire name");
}
