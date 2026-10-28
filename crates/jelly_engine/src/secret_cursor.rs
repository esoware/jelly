use std::collections::BTreeSet;
use std::sync::Arc;

use jelly_core::{CoreError, CursorBytes, CursorScope, ReservationSlot, ScanStream};
use jelly_vault::{BlobContent, BlobKind, EntryId, SecretBlob, UnlockedVault, VaultError};
use zeroize::Zeroizing;

use crate::engine::Inner;
use crate::session::Session;

/// Frame version of a stored cursor. It's persisted in the vault, so later builds have
/// to read it.
const VERSION: u8 = 1;

/// Length of the little-endian scope id inside a frame.
const SCOPE_ID_BYTES: usize = 8;

/// The scope id a frame gives the wallet. Address row IDs start at 1, so it names no
/// address.
const WALLET_SCOPE_ID: i64 = 0;

/// Whether a scan stream lives in the vault instead of the wallet database.
///
/// The Monero scan blob is derived from the view key and carries every owned output's key
/// offset, which is what spends it, so no byte of it may reach `SQLite`.
pub(crate) const fn is_secret_scan(stream: ScanStream) -> bool {
    match stream {
        ScanStream::MoneroScan => true,
        ScanStream::BitcoinElectrumStatus
        | ScanStream::LitecoinElectrumStatus
        | ScanStream::DogecoinElectrumStatus
        | ScanStream::BitcoinCashElectrumStatus
        | ScanStream::ZcashTransactions
        | ScanStream::EvmLogScan
        | ScanStream::SolanaSignatures
        | ScanStream::TronTransfers
        | ScanStream::XrpTransactions
        | ScanStream::TonTransactions => false,
    }
}

/// The same decision for reservation slots. Monero's holds key images and the signed
/// bytes of sends still in flight.
pub(crate) const fn is_secret_slot(slot: ReservationSlot) -> bool {
    match slot {
        ReservationSlot::MoneroSends => true,
        ReservationSlot::ZcashSends
        | ReservationSlot::EvmNonces
        | ReservationSlot::SolanaSends
        | ReservationSlot::TronSends
        | ReservationSlot::XrpSends
        | ReservationSlot::TonSends => false,
    }
}

/// The open session of `wallet_id`. It doesn't mark activity, because persistence is
/// background work and mustn't hold a session open.
///
/// Any other session is refused, so a stale adapter can't write one wallet's output keys
/// into another wallet's vault.
fn secret_session(inner: &Inner, wallet_id: i64) -> Result<Arc<Session>, CoreError> {
    let session = inner.session.current().ok_or(CoreError::Locked)?;
    if session.wallet_id() == wallet_id { Ok(session) } else { Err(CoreError::Locked) }
}

/// Every stored entry inside `scopes` whose label is one of `wanted`.
///
/// `wanted` pairs each label with the caller's own value, so a match returns that value
/// and nothing has to parse a label back into the enum it came from.
pub(crate) fn read_secrets<T: Copy>(
    inner: &Inner,
    wallet_id: i64,
    scopes: &[CursorScope],
    wanted: &[(&'static str, T)],
) -> Result<Vec<(CursorScope, T, CursorBytes)>, CoreError> {
    let session = secret_session(inner, wallet_id)?;
    session.vault_read(|vault| {
        check_frames(vault)?;
        let mut found = Vec::new();
        for (_, blob) in vault.entries_of_kind(BlobKind::MoneroScanState) {
            if let Some(entry) = blob.with_bytes(|bytes| {
                let frame = decode(bytes)?;
                if !scopes.contains(&frame.scope) {
                    return None;
                }
                let &(_, value) = wanted.iter().find(|&&(label, _)| label == frame.label)?;
                Some((frame.scope, value, CursorBytes::copied(frame.payload)))
            }) {
                found.push(entry);
            }
        }
        Ok(found)
    })
}

/// Replaces or inserts the vault entry for `(scope, label)`.
pub(crate) async fn write_secret(
    inner: &Inner,
    wallet_id: i64,
    scope: CursorScope,
    label: &'static str,
    value: &CursorBytes,
) -> Result<(), CoreError> {
    let framed = value.with_bytes(|bytes| encode(scope, label, bytes)).ok_or_else(|| {
        CoreError::internal_without_source("a secret cursor does not fit its frame")
    })?;
    let session = secret_session(inner, wallet_id)?;
    session
        .vault_write(move |vault| {
            check_frames(vault)?;
            let existing = entry_for(vault, scope, label);
            let blob = SecretBlob::new(BlobContent::MoneroScanState, framed);
            match existing {
                Some(id) => vault.replace(id, blob),
                None => vault.insert(blob).map(|_| ()),
            }
        })
        .await
}

/// Every frame must decode and route to its own `(scope, label)` before any secret state
/// is read or replaced.
fn check_frames(vault: &UnlockedVault) -> Result<(), VaultError> {
    let mut scopes = BTreeSet::new();
    for (_, blob) in vault.entries_of_kind(BlobKind::MoneroScanState) {
        let scope = blob.with_bytes(|bytes| {
            let frame = decode(bytes)
                .ok_or(VaultError::Corrupt { reason: "a secret state frame is unreadable" })?;
            let is_routed = ScanStream::parse(frame.label).is_some_and(is_secret_scan)
                || ReservationSlot::parse(frame.label).is_some_and(is_secret_slot);
            if !is_routed {
                return Err(VaultError::Corrupt {
                    reason: "a secret state frame has an unknown route",
                });
            }
            Ok((frame.scope, frame.label.to_owned()))
        })?;
        if !scopes.insert(scope) {
            return Err(VaultError::Corrupt { reason: "secret state frames share a route" });
        }
    }
    Ok(())
}

/// The entry holding `(scope, label)`, if there is one.
fn entry_for(vault: &UnlockedVault, scope: CursorScope, label: &str) -> Option<EntryId> {
    vault.entries_of_kind(BlobKind::MoneroScanState).find_map(|(id, blob)| {
        blob.with_bytes(|bytes| {
            matches!(decode(bytes), Some(frame) if frame.scope == scope && frame.label == label)
        })
        .then_some(id)
    })
}

/// One secret cursor, borrowed from the vault blob it was read from.
///
/// Each `(scope, label)` is one `BlobKind::MoneroScanState` entry, framed as
/// `version:u8 | scope_id:i64 le | label_length:u8 | label | payload`, where the scope id is
/// an address row ID or [`WALLET_SCOPE_ID`]. Vault entry IDs are random, so the lookup key
/// has to live in the frame. Stream and reservation labels never overlap.
struct Frame<'a> {
    scope: CursorScope,
    label: &'a str,
    payload: &'a [u8],
}

/// `None` when the frame can't hold the cursor.
///
/// That's a label longer than its one length byte, a total that overflows a `usize`, or
/// an address whose ID would read back as the wallet. None of these happen with this
/// crate's labels and real address rows.
fn encode(scope: CursorScope, label: &str, value: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let scope_id = match scope {
        CursorScope::Wallet => WALLET_SCOPE_ID,
        CursorScope::Address(WALLET_SCOPE_ID) => return None,
        CursorScope::Address(address_id) => address_id,
    };
    let label = label.as_bytes();
    let label_length = u8::try_from(label.len()).ok()?;
    let total = 1usize
        .checked_add(SCOPE_ID_BYTES)?
        .checked_add(1)?
        .checked_add(label.len())?
        .checked_add(value.len())?;
    let mut encoded = Zeroizing::new(Vec::with_capacity(total));
    encoded.push(VERSION);
    encoded.extend_from_slice(&scope_id.to_le_bytes());
    encoded.push(label_length);
    encoded.extend_from_slice(label);
    encoded.extend_from_slice(value);
    Some(encoded)
}

fn decode(bytes: &[u8]) -> Option<Frame<'_>> {
    let (&version, rest) = bytes.split_first()?;
    if version != VERSION {
        return None;
    }
    let (id_bytes, rest) = rest.split_at_checked(SCOPE_ID_BYTES)?;
    let scope = match i64::from_le_bytes(<[u8; SCOPE_ID_BYTES]>::try_from(id_bytes).ok()?) {
        WALLET_SCOPE_ID => CursorScope::Wallet,
        address_id => CursorScope::Address(address_id),
    };
    let (&label_length, rest) = rest.split_first()?;
    let (label, payload) = rest.split_at_checked(usize::from(label_length))?;
    Some(Frame { scope, label: str::from_utf8(label).ok()?, payload })
}

#[cfg(test)]
mod tests {
    use jelly_chains::AdapterFactory;
    use jelly_core::{
        AdapterBuilder as _, BuildRequest, ChainId, ChainOutcome, SkipReason,
        test_util::build_request,
    };
    use jelly_net::test_util::direct_network;

    use super::*;
    use crate::fixtures::{harness, seed_wallet};

    const LABEL: &str = ScanStream::MoneroScan.as_str();

    /// The routing decision, in one place and exhaustive over both vocabularies.
    #[test]
    fn only_monero_is_secret() {
        let secret: Vec<&str> = ScanStream::ALL
            .into_iter()
            .filter(|&stream| is_secret_scan(stream))
            .map(ScanStream::as_str)
            .chain(
                ReservationSlot::ALL
                    .into_iter()
                    .filter(|&slot| is_secret_slot(slot))
                    .map(ReservationSlot::as_str),
            )
            .collect();
        assert_eq!(secret, vec!["monero.scan", "monero.sends"]);
    }

    fn round_trip(framed: &[u8]) -> Option<(CursorScope, String, Vec<u8>)> {
        decode(framed).map(|frame| (frame.scope, frame.label.to_owned(), frame.payload.to_vec()))
    }

    #[test]
    fn a_frame_round_trips() {
        let scope = CursorScope::Address(42);
        let framed = encode(scope, LABEL, b"scan state").expect("the label fits");
        assert_eq!(round_trip(&framed), Some((scope, LABEL.to_owned(), b"scan state".to_vec())));
    }

    #[test]
    fn an_empty_payload_round_trips() {
        let scope = CursorScope::Address(-1);
        let framed = encode(scope, LABEL, &[]).expect("the label fits");
        assert_eq!(round_trip(&framed), Some((scope, LABEL.to_owned(), Vec::new())));
    }

    /// The wallet is framed as scope zero, so no address can be framed as zero too.
    #[test]
    fn the_wallet_is_scope_zero_and_no_address_can_claim_it() {
        let framed = encode(CursorScope::Wallet, LABEL, b"x").expect("the label fits");
        assert_eq!(framed.get(1..=SCOPE_ID_BYTES), Some(WALLET_SCOPE_ID.to_le_bytes().as_slice()));
        assert!(encode(CursorScope::Address(WALLET_SCOPE_ID), LABEL, b"x").is_none());
    }

    #[test]
    fn unreadable_frames_are_refused() {
        assert!(decode(&[]).is_none());
        assert!(decode(&[VERSION]).is_none());
        // A frame from a future format is refused rather than misread.
        assert!(decode(&[VERSION.saturating_add(1)]).is_none());
    }

    #[test]
    fn a_truncated_label_is_refused() {
        let mut framed =
            encode(CursorScope::Address(1), LABEL, b"x").expect("the label fits").to_vec();
        framed.truncate(1 + SCOPE_ID_BYTES + 1 + 1);
        assert!(decode(&framed).is_none());
    }

    /// Streams and reservations share the frame, so both kinds of label have to come back
    /// as themselves.
    #[test]
    fn a_reservation_frames_the_same_way() {
        let label = ReservationSlot::MoneroSends.as_str();
        let framed = encode(CursorScope::Wallet, label, b"key images").expect("the label fits");
        assert_eq!(
            round_trip(&framed),
            Some((CursorScope::Wallet, label.to_owned(), b"key images".to_vec()))
        );
    }

    /// One frame the vault cannot route leaves no secret cursor trustworthy.
    ///
    /// Every secret read and write fails, the chain that needs them is not built, and the
    /// frame itself stays as it was.
    #[tokio::test]
    async fn a_bad_secret_frame_refuses_every_secret_read_and_write_and_the_monero_build() {
        for bytes in [
            vec![99],
            encode(CursorScope::Wallet, "unknown.route", b"reservation").unwrap().to_vec(),
            vec![VERSION],
        ] {
            let harness = harness().await;
            let wallet = seed_wallet(&harness.engine, "frames").await;
            let session = harness.engine.inner.session.current().unwrap();
            let cursors = harness.engine.cursors(wallet.id);
            let factory = AdapterFactory::new();
            factory.attach(direct_network(jelly_chains::chains().entries()).unwrap());
            let request = BuildRequest {
                cursors: Arc::clone(&cursors),
                account_keys: session.account_keys(),
                ..build_request(&[ChainId::Monero, ChainId::Bitcoin])
            };
            assert!(factory.build(&request).await.adapter(ChainId::Monero).is_some());
            let framed = Zeroizing::new(bytes.clone());
            let id = session
                .vault_write(move |vault| {
                    vault.insert(SecretBlob::new(BlobContent::MoneroScanState, framed))
                })
                .await
                .unwrap();
            for _ in 0..2 {
                cursors
                    .load_reservation(CursorScope::Wallet, ReservationSlot::MoneroSends)
                    .await
                    .unwrap_err();
                cursors.load_scan(CursorScope::Wallet, ScanStream::MoneroScan).await.unwrap_err();
                assert!(
                    cursors
                        .reserve(
                            CursorScope::Wallet,
                            ReservationSlot::MoneroSends,
                            CursorBytes::copied(b"replacement")
                        )
                        .await
                        .is_err()
                );
                assert!(
                    write_secret(
                        &harness.engine.inner,
                        wallet.id,
                        CursorScope::Wallet,
                        ScanStream::MoneroScan.as_str(),
                        &CursorBytes::copied(b"replacement")
                    )
                    .await
                    .is_err()
                );
                let built = factory.build(&request).await;
                assert!(matches!(
                    built.outcome(ChainId::Monero),
                    Some(ChainOutcome::Skipped(SkipReason::BuildFailed))
                ));
                assert!(built.adapter(ChainId::Bitcoin).is_some());
            }
            session
                .vault_read(|vault| {
                    assert!(vault.read(id)?.with_bytes(|stored| stored == bytes));
                    assert!(vault.read_first(BlobKind::SeedEntropy).is_some());
                    Ok(())
                })
                .unwrap();
        }
    }
}
