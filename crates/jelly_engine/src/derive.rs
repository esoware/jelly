use std::sync::Arc;

use jelly_core::{CoreError, KeyMaterial, KeySource};
use jelly_keypair::{parse_path, purpose_of};
use jelly_store::AddressRow;
use jelly_vault::EntryId;

use crate::error::blocking;
use crate::session::Session;

/// The address a row's key produces. A row is a claim, and the key behind it is the
/// proof.
pub(crate) fn expected_address(session: &Session, row: &AddressRow) -> Result<String, CoreError> {
    let entry = session.entry(row.chain);
    let spec = entry.spec();
    match row.source {
        KeySource::Derived { ref path } => {
            let path = parse_path(path.as_str())?;
            let script = spec.resolve_script(purpose_of(&path), row.script_kind)?;
            let key = session.keychain().key_material_at(spec.key_kind, &path)?;
            Ok(entry.address(&key, script)?)
        }
        KeySource::Imported { ref vault_entry } => {
            let script = spec.resolve_script(None, row.script_kind)?;
            Ok(entry.address(&imported_key(session, vault_entry)?, script)?)
        }
        KeySource::Subaddress { major, minor } => {
            Ok(entry.subaddress(session.account_key(row.chain)?, major, minor)?)
        }
    }
}

/// The address a row claims must be the address its key produces.
fn verify_row(session: &Session, row: &AddressRow) -> Result<(), CoreError> {
    if expected_address(session, row)? == row.address {
        Ok(())
    } else {
        Err(CoreError::AddressMismatch { address_id: row.id })
    }
}

/// Verifies a whole list on the blocking pool.
///
/// One row takes microseconds, but a wallet's worth is milliseconds of key derivation,
/// which does not belong on a runtime worker.
pub(crate) async fn verify_rows(
    session: Arc<Session>,
    rows: Vec<AddressRow>,
) -> Result<Vec<AddressRow>, CoreError> {
    if rows.is_empty() {
        return Ok(rows);
    }
    blocking(move || {
        for row in &rows {
            verify_row(&session, row)?;
        }
        Ok::<_, CoreError>(rows)
    })
    .await
}

/// The signing key behind a row.
pub(crate) fn resolve_key(session: &Session, row: &AddressRow) -> Result<KeyMaterial, CoreError> {
    match row.source {
        KeySource::Derived { ref path } => {
            let path = parse_path(path.as_str())?;
            let kind = session.entry(row.chain).spec().key_kind;
            Ok(session.keychain().key_material_at(kind, &path)?)
        }
        KeySource::Imported { ref vault_entry } => imported_key(session, vault_entry),
        KeySource::Subaddress { major, minor } => {
            let account = session.account_key(row.chain)?;
            Ok(session.entry(row.chain).subaddress_key(account, major, minor)?)
        }
    }
}

fn imported_key(session: &Session, vault_entry: &str) -> Result<KeyMaterial, CoreError> {
    let entry = EntryId::parse(vault_entry)?;
    session.vault_read(|vault| vault.key_material(entry))
}
