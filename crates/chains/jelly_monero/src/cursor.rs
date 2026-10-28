use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use jelly_core::{CursorBytes, CursorScope, CursorStore};
use monero_wallet::WalletOutput;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use crate::byte_count::ByteCount;
use crate::error::MoneroError;
use crate::params::{SCAN_STREAM, SENDS_SLOT};
use crate::pending::{InFlight, TerminalSend};
use crate::reader::Reader;
use crate::state::{BlockRef, OwnedOutput, ScanState, Spend};
use crate::subaddress::Subaddress;

const TARGET: &str = "jelly::monero::cursor";

const SCAN_MAGIC: &[u8; 8] = b"JLYXMRSC";
const SCAN_VERSION: u16 = 2;

const SENDS_MAGIC: &[u8; 8] = b"JLYXMRIF";
const SENDS_VERSION: u16 = 1;

// Two disk formats, both little endian. They're separate blobs because the scan blob
// is progress, committed behind the rows its pass emitted, while the sends blob is a
// reservation, durable before any signed bytes leave the process.
//
// Scan blob, after the eight-byte magic and the u16 version:
//
//   account public spend key (32), restore height (8), next height (8),
//   tip height (8), lookahead major (4), lookahead minor (4),
//   settled-through checkpoint (8),
//   recent block count (4), then per block: height (8), block id (32),
//   output count (4), then per output: length-prefixed (4) WalletOutput bytes,
//     height (8), on-chain timestamp (8), key image (32), spend presence (1)
//     and 32 bytes of transaction id plus a height (8) when present.
//
// Sends blob, after its own magic and version:
//
//   account public spend key (32), in-flight count (4), then per send:
//     transaction id (32), send height (8), amount (8),
//     subaddress major (4) and minor (4),
//     key-image count (4) and 32 bytes each, recipient length (2) and its utf-8
//     bytes, ring endpoint length (2) and its utf-8 bytes, signed transaction
//     length (4) and its bytes.
//   terminal count (4), then per terminal send: transaction id (32), checkpoint (8),
//     subaddress major (4) and minor (4), amount (8), failure flag (1),
//     recipient length (2) and its utf-8 bytes.
//   ring count (4), then per ring: the spent output's key image (32), member
//     count (1), and each member's position on the blockchain (8), ascending.

/// The scan blob up to its recent block count.
const SCAN_HEADER_BYTES: usize = 82;

/// One block id and its height.
const BLOCK_REF_BYTES: usize = 40;

/// One output's height, timestamp, key image, and spend presence flag.
const OUTPUT_BOOKKEEPING_BYTES: usize = 49;

/// The sends blob up to its first send.
const SENDS_HEADER_BYTES: usize = 46;

/// One send's fixed fields: the transaction id, the height, the amount, the
/// subaddress, and the four count and length prefixes.
const SEND_FIXED_BYTES: usize = 68;
const TERMINAL_FIXED_BYTES: usize = 59;

/// One ring's key image and member count.
const RING_FIXED_BYTES: usize = 33;

/// Serializes the scan state without the sends.
///
/// The blob is sized before anything is written, so it never grows and leaves no
/// copy of a key offset behind, and each output is written straight into it.
fn encode_scan(state: &ScanState) -> Result<Vec<u8>, MoneroError> {
    let mut size = SCAN_HEADER_BYTES
        .saturating_add(4)
        .saturating_add(state.recent_blocks().count().saturating_mul(BLOCK_REF_BYTES))
        .saturating_add(4);
    for owned in state.outputs() {
        let mut counted = ByteCount::default();
        owned.output().write(&mut counted).map_err(|_| unserializable())?;
        let spend = owned.spend().map_or(0, |_| 40);
        size = size
            .saturating_add(4)
            .saturating_add(counted.total())
            .saturating_add(OUTPUT_BOOKKEEPING_BYTES)
            .saturating_add(spend);
    }

    let mut encoded = Vec::with_capacity(size);
    encoded.extend_from_slice(SCAN_MAGIC);
    encoded.extend_from_slice(&SCAN_VERSION.to_le_bytes());
    encoded.extend_from_slice(&state.public_spend());
    encoded.extend_from_slice(&state.restore_height().to_le_bytes());
    encoded.extend_from_slice(&state.next_height().to_le_bytes());
    encoded.extend_from_slice(&state.tip_height().to_le_bytes());
    let (major, minor) = state.lookahead();
    encoded.extend_from_slice(&major.to_le_bytes());
    encoded.extend_from_slice(&minor.to_le_bytes());
    encoded.extend_from_slice(&state.settled_through.to_le_bytes());

    let recent_count = persisted_count(state.recent_blocks().count(), "too many block ids")?;
    encoded.extend_from_slice(&recent_count.to_le_bytes());
    for block in state.recent_blocks() {
        encoded.extend_from_slice(&block.height.to_le_bytes());
        encoded.extend_from_slice(&block.hash);
    }

    let output_count = persisted_count(state.outputs().len(), "too many outputs")?;
    encoded.extend_from_slice(&output_count.to_le_bytes());
    for owned in state.outputs() {
        let length_at = encoded.len();
        encoded.extend_from_slice(&[0; 4]);
        owned.output().write(&mut encoded).map_err(|_| unserializable())?;
        let length = persisted_count(
            encoded.len().saturating_sub(length_at).saturating_sub(4),
            "an output is too large",
        )?;
        encoded
            .get_mut(length_at..length_at.saturating_add(4))
            .ok_or_else(unserializable)?
            .copy_from_slice(&length.to_le_bytes());
        encoded.extend_from_slice(&owned.height().to_le_bytes());
        encoded.extend_from_slice(&owned.timestamp_secs().to_le_bytes());
        encoded.extend_from_slice(&owned.key_image());
        match owned.spend() {
            Some(spend) => {
                encoded.push(1);
                encoded.extend_from_slice(&spend.txid);
                encoded.extend_from_slice(&spend.height.to_le_bytes());
            }
            None => encoded.push(0),
        }
    }

    Ok(encoded)
}

/// Serializes the sends in flight, the terminal sends, and the rings, sized up front like
/// the scan blob.
fn encode_sends(state: &ScanState) -> Result<Vec<u8>, MoneroError> {
    let size = state.in_flight_sends().fold(SENDS_HEADER_BYTES, |size, send| {
        size.saturating_add(SEND_FIXED_BYTES)
            .saturating_add(send.key_images.len().saturating_mul(32))
            .saturating_add(send.recipient.len())
            .saturating_add(send.endpoint.len())
            .saturating_add(send.raw.len())
    });
    let size = state.terminal_sends.iter().fold(size.saturating_add(4), |size, send| {
        size.saturating_add(TERMINAL_FIXED_BYTES).saturating_add(send.recipient.len())
    });
    let size = state.rings.values().fold(size.saturating_add(4), |size, positions| {
        size.saturating_add(RING_FIXED_BYTES).saturating_add(positions.len().saturating_mul(8))
    });
    let mut encoded = Vec::with_capacity(size);
    encoded.extend_from_slice(SENDS_MAGIC);
    encoded.extend_from_slice(&SENDS_VERSION.to_le_bytes());
    encoded.extend_from_slice(&state.public_spend());
    let send_count = persisted_count(state.in_flight_count(), "too many transactions in flight")?;
    encoded.extend_from_slice(&send_count.to_le_bytes());
    for send in state.in_flight_sends() {
        encoded.extend_from_slice(&send.txid);
        encoded.extend_from_slice(&send.sent_at_height.to_le_bytes());
        encoded.extend_from_slice(&send.amount_piconero.to_le_bytes());
        encoded.extend_from_slice(&send.subaddress.major.to_le_bytes());
        encoded.extend_from_slice(&send.subaddress.minor.to_le_bytes());
        let image_count = persisted_count(send.key_images.len(), "too many key images")?;
        encoded.extend_from_slice(&image_count.to_le_bytes());
        for image in &send.key_images {
            encoded.extend_from_slice(image);
        }
        write_text(&mut encoded, &send.recipient, "a recipient is too long")?;
        write_text(&mut encoded, &send.endpoint, "a ring endpoint is too long")?;
        let raw_length = persisted_count(send.raw.len(), "a signed transaction is too large")?;
        encoded.extend_from_slice(&raw_length.to_le_bytes());
        encoded.extend_from_slice(&send.raw);
    }
    encoded.extend_from_slice(
        &persisted_count(state.terminal_sends.len(), "too many terminal sends")?.to_le_bytes(),
    );
    for send in &state.terminal_sends {
        encoded.extend_from_slice(&send.txid);
        encoded.extend_from_slice(&send.checkpoint.to_le_bytes());
        encoded.extend_from_slice(&send.subaddress.major.to_le_bytes());
        encoded.extend_from_slice(&send.subaddress.minor.to_le_bytes());
        encoded.extend_from_slice(&send.amount_piconero.to_le_bytes());
        encoded.push(u8::from(send.is_failed));
        write_text(&mut encoded, &send.recipient, "a recipient is too long")?;
    }
    encoded.extend_from_slice(&persisted_count(state.rings.len(), "too many rings")?.to_le_bytes());
    for (key_image, positions) in &state.rings {
        encoded.extend_from_slice(key_image);
        let count = u8::try_from(positions.len())
            .map_err(|_| MoneroError::TooLargeToPersist { what: "a ring is too large" })?;
        encoded.push(count);
        for position in positions {
            encoded.extend_from_slice(&position.to_le_bytes());
        }
    }
    Ok(encoded)
}

/// Reads back the scan state, refusing a blob written for another account.
fn decode_scan(blob: &[u8], public_spend: [u8; 32]) -> Result<ScanState, MoneroError> {
    let mut reader = Reader::new(blob, ends_early);
    if reader.take(SCAN_MAGIC.len())? != SCAN_MAGIC.as_slice() {
        return Err(MoneroError::Blob { what: "it is not monero scan state" });
    }
    if reader.u16()? != SCAN_VERSION {
        return Err(MoneroError::Blob { what: "it was written by another version" });
    }
    if reader.array32()? != public_spend {
        return Err(MoneroError::Blob { what: "it belongs to a different monero account" });
    }
    let restore_height = reader.u64()?;
    let next_height = reader.u64()?;
    let tip_height = reader.u64()?;
    let lookahead = (reader.u32()?, reader.u32()?);
    let settled_through = reader.u64()?;

    let mut state = ScanState::new(public_spend, restore_height, lookahead);
    state.settled_through = settled_through;
    state.advance_to(next_height);
    state.set_tip_height(tip_height);

    let recent_count = reader.u32()?;
    let mut recent_blocks = VecDeque::new();
    for _ in 0..recent_count {
        let height = reader.u64()?;
        let hash = reader.array32()?;
        recent_blocks.push_back(BlockRef { height, hash });
    }

    let output_count = reader.u32()?;
    let mut outputs = Vec::new();
    for _ in 0..output_count {
        let length = read_length(&mut reader)?;
        let mut frame = reader.take(length)?;
        let output = WalletOutput::read(&mut frame)
            .map_err(|_| MoneroError::Blob { what: "an output does not decode" })?;
        if !frame.is_empty() {
            return Err(MoneroError::Blob { what: "an output frame has trailing bytes" });
        }
        let height = reader.u64()?;
        let timestamp_secs = reader.u64()?;
        let key_image = reader.array32()?;
        let spend = if reader.u8()? == 1 {
            Some(Spend { txid: reader.array32()?, height: reader.u64()? })
        } else {
            None
        };
        outputs.push(OwnedOutput::new(output, height, timestamp_secs, key_image, spend));
    }

    if !reader.is_empty() {
        return Err(MoneroError::Blob { what: "it has trailing bytes" });
    }
    state
        .with_scanned(recent_blocks, outputs)
        .ok_or(MoneroError::Blob { what: "it names the same output twice" })
}

/// What the sends blob holds.
#[derive(Debug)]
struct Sends {
    in_flight: Vec<InFlight>,
    terminal: Vec<TerminalSend>,
    rings: BTreeMap<[u8; 32], Vec<u64>>,
}

impl Sends {
    fn restore_into(self, state: &mut ScanState) {
        for send in self.in_flight {
            state.track(send);
        }
        state.terminal_sends = self.terminal;
        state.rings = self.rings;
    }
}

/// Refuses a blob written for another account, whose sends would otherwise be followed
/// up as this one's.
fn decode_sends(blob: &[u8], public_spend: [u8; 32]) -> Result<Sends, MoneroError> {
    let mut reader = Reader::new(blob, ends_early);
    if reader.take(SENDS_MAGIC.len())? != SENDS_MAGIC.as_slice() {
        return Err(MoneroError::Blob { what: "it is not monero sends in flight" });
    }
    if reader.u16()? != SENDS_VERSION {
        return Err(MoneroError::Blob { what: "it was written by another version" });
    }
    if reader.array32()? != public_spend {
        return Err(MoneroError::Blob { what: "it belongs to a different monero account" });
    }
    let send_count = read_length(&mut reader)?;
    if send_count > blob.len().saturating_sub(SENDS_HEADER_BYTES).saturating_div(SEND_FIXED_BYTES) {
        return Err(ends_early());
    }
    let mut in_flight = Vec::with_capacity(send_count);
    for _ in 0..send_count {
        let txid = reader.array32()?;
        let sent_at_height = reader.u64()?;
        let amount_piconero = reader.u64()?;
        let subaddress = Subaddress::new(reader.u32()?, reader.u32()?);
        let image_count = read_length(&mut reader)?;
        if image_count > blob.len().saturating_div(32) {
            return Err(ends_early());
        }
        let mut key_images = Vec::with_capacity(image_count);
        for _ in 0..image_count {
            key_images.push(reader.array32()?);
        }
        let recipient = reader.text(MoneroError::Blob { what: "a recipient is not text" })?;
        let endpoint = reader.text(MoneroError::Blob { what: "a ring endpoint is not text" })?;
        let raw_length = read_length(&mut reader)?;
        let raw = reader.take(raw_length)?.to_vec();
        in_flight.push(InFlight {
            txid,
            key_images,
            raw,
            sent_at_height,
            amount_piconero,
            recipient,
            subaddress,
            endpoint,
        });
    }
    let terminal_count = read_length(&mut reader)?;
    if terminal_count > blob.len().saturating_div(TERMINAL_FIXED_BYTES) {
        return Err(ends_early());
    }
    let mut terminal = Vec::with_capacity(terminal_count);
    for _ in 0..terminal_count {
        let txid = reader.array32()?;
        let checkpoint = reader.u64()?;
        let subaddress = Subaddress::new(reader.u32()?, reader.u32()?);
        let amount_piconero = reader.u64()?;
        let is_failed = match reader.u8()? {
            0 => false,
            1 => true,
            _ => return Err(MoneroError::Blob { what: "a terminal outcome is invalid" }),
        };
        let recipient = reader.text(MoneroError::Blob { what: "a recipient is not text" })?;
        if checkpoint == 0 {
            return Err(MoneroError::Blob { what: "a terminal checkpoint is invalid" });
        }
        terminal.push(TerminalSend {
            txid,
            subaddress,
            amount_piconero,
            recipient,
            checkpoint,
            is_failed,
        });
    }
    let ring_count = read_length(&mut reader)?;
    if ring_count > blob.len().saturating_div(RING_FIXED_BYTES) {
        return Err(ends_early());
    }
    let mut rings = BTreeMap::new();
    for _ in 0..ring_count {
        let key_image = reader.array32()?;
        let member_count = reader.u8()?;
        let mut positions = Vec::with_capacity(usize::from(member_count));
        for _ in 0..member_count {
            positions.push(reader.u64()?);
        }
        if positions.is_empty() || !positions.is_sorted_by(|before, after| before < after) {
            return Err(MoneroError::Blob { what: "a ring is not in ascending order" });
        }
        if rings.insert(key_image, positions).is_some() {
            return Err(MoneroError::Blob { what: "it names one ring twice" });
        }
    }
    if !reader.is_empty() {
        return Err(MoneroError::Blob { what: "it has trailing bytes" });
    }
    Ok(Sends { in_flight, terminal, rings })
}

/// Reads and writes the two persisted blobs.
///
/// Both are secret, so the cursor port routes both to the vault. The scan blob
/// carries every output's key offset, and the sends blob carries key images, signed
/// transactions, and recipients.
#[derive(Debug)]
pub(crate) struct Blobs {
    cursors: Arc<dyn CursorStore>,
    /// Serializes writes of the sends blob. Each write encodes the whole set, so two
    /// overlapping writes could land out of order and drop a sent transaction, freeing
    /// its inputs for a rebuild that pays twice.
    writing_sends: AsyncMutex<()>,
}

impl Blobs {
    pub(crate) fn new(cursors: Arc<dyn CursorStore>) -> Self {
        Self { cursors, writing_sends: AsyncMutex::new(()) }
    }

    /// An unreadable scan blob only costs a rescan, but an unreadable sends blob fails
    /// the restore, so no adapter spends without its reservations.
    ///
    /// A lower restore height or a wider subaddress window restarts the scan, and the
    /// sends are restored either way, so a rescan can't lose a broadcast's follow-up.
    pub(crate) async fn restore(
        &self,
        public_spend: [u8; 32],
        restore_height: u64,
        lookahead: (u32, u32),
    ) -> Result<ScanState, MoneroError> {
        let mut state = self.restore_scan(public_spend, restore_height, lookahead).await?;
        let held = self
            .cursors
            .load_reservation(CursorScope::Wallet, SENDS_SLOT)
            .await
            .map_err(MoneroError::Cursor)?;
        if let Some(blob) = held {
            blob.with_bytes(|bytes| decode_sends(bytes, public_spend))?.restore_into(&mut state);
        }
        Ok(state)
    }

    pub(crate) async fn restore_scan(
        &self,
        public_spend: [u8; 32],
        restore_height: u64,
        lookahead: (u32, u32),
    ) -> Result<ScanState, MoneroError> {
        let fresh = || ScanState::new(public_spend, restore_height, lookahead);
        let stored = self
            .cursors
            .load_scan(CursorScope::Wallet, SCAN_STREAM)
            .await
            .map_err(MoneroError::Cursor)?;
        let decoded = stored.map(|blob| blob.with_bytes(|bytes| decode_scan(bytes, public_spend)));
        let mut state = match decoded {
            None => fresh(),
            Some(Ok(state)) => state,
            Some(Err(error)) => {
                tracing::warn!(
                    target: TARGET,
                    %error,
                    "discarding the monero scan state; rescanning from the birthday"
                );
                fresh()
            }
        };
        state.set_restore_height(restore_height);
        let (major, minor) = lookahead;
        let last_in_window = Subaddress::new(major.saturating_sub(1), minor.saturating_sub(1));
        if state.widen(last_in_window, 1) {
            state.reset_scan();
        }

        Ok(state)
    }

    /// Goes through `advance`, so it's durable only once the rows its pass emitted
    /// are. Encoded under the guard instead of cloning a state that holds every
    /// output the wallet owns.
    pub(crate) async fn write_scan(&self, state: &Mutex<ScanState>) -> Result<(), MoneroError> {
        let blob = encode_scan(&state.lock())?;
        self.cursors
            .advance(CursorScope::Wallet, SCAN_STREAM, CursorBytes::new(blob))
            .await
            .map_err(MoneroError::Cursor)
    }

    /// Returns only once the sends are durable. Nothing may be broadcast before it
    /// has returned `Ok`.
    pub(crate) async fn write_sends(&self, state: &Mutex<ScanState>) -> Result<(), MoneroError> {
        let _serialized = self.writing_sends.lock().await;
        let blob = encode_sends(&state.lock())?;
        self.cursors
            .reserve(CursorScope::Wallet, SENDS_SLOT, CursorBytes::new(blob))
            .await
            .map_err(MoneroError::Cursor)
    }
}

fn persisted_count(value: usize, what: &'static str) -> Result<u32, MoneroError> {
    u32::try_from(value).map_err(|_| MoneroError::TooLargeToPersist { what })
}

/// Writes `text` behind its `u16` length, which is how [`Reader::text`] reads it.
fn write_text(
    encoded: &mut Vec<u8>,
    text: &str,
    too_long: &'static str,
) -> Result<(), MoneroError> {
    let length =
        u16::try_from(text.len()).map_err(|_| MoneroError::TooLargeToPersist { what: too_long })?;
    encoded.extend_from_slice(&length.to_le_bytes());
    encoded.extend_from_slice(text.as_bytes());
    Ok(())
}

const fn unserializable() -> MoneroError {
    MoneroError::Internal { context: "an output did not serialize" }
}

const fn ends_early() -> MoneroError {
    MoneroError::Blob { what: "it ends early" }
}

/// A length prefix, refused when it can't address memory on this target.
fn read_length(reader: &mut Reader<'_>) -> Result<usize, MoneroError> {
    usize::try_from(reader.u32()?)
        .map_err(|_| MoneroError::Blob { what: "a length is out of range" })
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;
    use crate::fixtures::{PUBLIC_SPEND, recorded_outputs};

    fn state() -> ScanState {
        let mut state = ScanState::new(PUBLIC_SPEND, 2_400_000, (3, 7));
        state.advance_to(2_400_500);
        state.set_tip_height(2_400_499);
        state.settled_through = 5;
        state.terminal_sends.push(TerminalSend {
            txid: [0xde; 32],
            subaddress: Subaddress::new(0, 3),
            amount_piconero: 600,
            recipient: "4BBB".to_owned(),
            checkpoint: 6,
            is_failed: true,
        });
        state.note_block(BlockRef { height: 2_400_498, hash: [0xa1; 32] });
        state.note_block(BlockRef { height: 2_400_499, hash: [0xa2; 32] });
        state.track(InFlight {
            txid: [0xcd; 32],
            key_images: vec![[1; 32], [2; 32]],
            raw: vec![9u8; 64],
            sent_at_height: 2_400_490,
            amount_piconero: 12_345,
            recipient: "4AAA".to_owned(),
            subaddress: Subaddress::new(1, 2),
            endpoint: "https://node.invalid:18081".to_owned(),
        });
        state.rings.insert(
            [1; 32],
            (0..16u64).map(|member| member.saturating_mul(1_000).saturating_add(7)).collect(),
        );
        state.rings.insert([3; 32], vec![5, 6, 9]);
        state
    }

    /// Damaged copies of `blob`, each with the account it is decoded for and the
    /// reason the decoder has to give.
    fn damaged(
        blob: &[u8],
        magic_refusal: &'static str,
    ) -> Vec<(String, Vec<u8>, [u8; 32], &'static str)> {
        let mut wrong_magic = blob.to_vec();
        if let Some(byte) = wrong_magic.first_mut() {
            *byte = b'X';
        }
        let mut wrong_version = blob.to_vec();
        if let Some(byte) = wrong_version.get_mut(8) {
            *byte = 9;
        }
        let mut trailing = blob.to_vec();
        trailing.push(0);
        let mut cases = vec![
            ("wrong magic".to_owned(), wrong_magic, PUBLIC_SPEND, magic_refusal),
            (
                "wrong version".to_owned(),
                wrong_version,
                PUBLIC_SPEND,
                "it was written by another version",
            ),
            (
                "another account".to_owned(),
                blob.to_vec(),
                [0x22; 32],
                "it belongs to a different monero account",
            ),
            ("trailing bytes".to_owned(), trailing, PUBLIC_SPEND, "it has trailing bytes"),
            ("empty".to_owned(), Vec::new(), PUBLIC_SPEND, "it ends early"),
        ];
        for truncate_at in [8usize, 12, 40, 60, 90] {
            let short = blob.get(..truncate_at.min(blob.len())).unwrap_or_default().to_vec();
            cases.push((
                format!("truncated at {truncate_at}"),
                short,
                PUBLIC_SPEND,
                "it ends early",
            ));
        }
        cases
    }

    fn assert_refused<T: fmt::Debug>(decoded: Result<T, MoneroError>, expected: &str, case: &str) {
        match decoded {
            Err(MoneroError::Blob { what }) => assert_eq!(what, expected, "{case}"),
            other => panic!("{case}: expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_scan_and_sends_blobs_round_trip_byte_for_byte() {
        let state = state();
        let blob = encode_scan(&state).expect("it serializes");
        let mut restored = decode_scan(&blob, PUBLIC_SPEND).expect("it restores");
        assert_eq!(restored.restore_height(), state.restore_height());
        assert_eq!(restored.next_height(), state.next_height());
        assert_eq!(restored.tip_height(), state.tip_height());
        assert_eq!(restored.settled_through, state.settled_through);
        assert_eq!(restored.lookahead(), (3, 7));
        assert_eq!(restored.recent_blocks().count(), 2);
        assert_eq!(encode_scan(&restored).expect("it serializes"), blob);

        // The sends are their own blob, so the scan blob restores without them.
        assert_eq!(restored.in_flight_sends().count(), 0);
        let sends = encode_sends(&state).expect("it serializes");
        decode_sends(&sends, PUBLIC_SPEND).expect("it restores").restore_into(&mut restored);
        assert_eq!(restored.terminal_sends, state.terminal_sends);
        assert_eq!(restored.rings, state.rings);
        assert_eq!(restored.in_flight_sends().count(), 1);
        assert_eq!(restored.in_flight_sends().next(), state.in_flight_sends().next());
        assert_eq!(encode_sends(&restored).expect("it serializes"), encode_sends(&state).unwrap());
    }

    /// A blob that grew while it was written would have left a copy of what it
    /// held in the allocation it moved out of.
    #[test]
    fn both_blobs_are_written_into_a_buffer_of_their_final_size() {
        let mut state = state();
        let outputs = recorded_outputs();
        assert!(!outputs.is_empty(), "the fixture pays its account");
        for owned in outputs {
            state.record_output(owned);
        }
        let blob = encode_scan(&state).expect("it serializes");
        assert_eq!(blob.capacity(), blob.len());
        let restored = decode_scan(&blob, PUBLIC_SPEND).expect("it restores");
        assert_eq!(restored.outputs().len(), state.outputs().len());
        assert_eq!(encode_scan(&restored).expect("it serializes"), blob);

        let sends = encode_sends(&state).expect("it serializes");
        assert_eq!(sends.capacity(), sends.len());
    }

    /// The scan blob holds no signed transaction, so a rescan can neither lose nor
    /// roll back a payment in flight.
    #[test]
    fn the_scan_blob_holds_no_send() {
        let state = state();
        let blob = encode_scan(&state).expect("it serializes");
        let raw = vec![9u8; 64];
        assert!(
            !blob.windows(raw.len()).any(|window| window == raw.as_slice()),
            "a signed transaction reached the scan blob"
        );
        assert!(
            encode_sends(&state)
                .expect("it serializes")
                .windows(raw.len())
                .any(|window| window == raw.as_slice()),
            "the sends blob is where it belongs"
        );
    }

    #[test]
    fn a_sends_blob_is_refused_on_magic_version_account_or_trailing_bytes() {
        let blob = encode_sends(&state()).expect("it serializes");
        assert_eq!(decode_sends(&blob, PUBLIC_SPEND).expect("it restores").in_flight.len(), 1);
        for (case, damaged, public_spend, expected) in
            damaged(&blob, "it is not monero sends in flight")
        {
            assert_refused(decode_sends(&damaged, public_spend), expected, &case);
        }
        // Neither blob may be read as the other.
        let scan = encode_scan(&state()).expect("it serializes");
        assert_refused(
            decode_sends(&scan, PUBLIC_SPEND),
            "it is not monero sends in flight",
            "a scan blob",
        );
        assert_refused(
            decode_scan(&blob, PUBLIC_SPEND),
            "it is not monero scan state",
            "a sends blob",
        );
    }

    #[test]
    fn send_counts_cannot_allocate_more_than_the_frame_contains() {
        let blob = encode_sends(&state()).unwrap();
        for count_at in [42usize, 102] {
            let mut damaged = blob.clone();
            damaged[count_at..count_at.saturating_add(4)].copy_from_slice(&u32::MAX.to_le_bytes());
            assert_refused(
                decode_sends(&damaged, PUBLIC_SPEND),
                "it ends early",
                "an oversized count",
            );
        }
    }

    #[test]
    fn a_scan_blob_is_refused_on_magic_version_account_or_trailing_bytes() {
        let blob = encode_scan(&state()).expect("it serializes");
        for (case, damaged, public_spend, expected) in damaged(&blob, "it is not monero scan state")
        {
            assert_refused(decode_scan(&damaged, public_spend), expected, &case);
        }
        assert_refused(decode_scan(b"{}", PUBLIC_SPEND), "it ends early", "two bytes of json");
    }

    #[test]
    fn an_empty_state_round_trips_too() {
        let state = ScanState::new(PUBLIC_SPEND, 0, (1, 1));
        let blob = encode_scan(&state).expect("it serializes");
        let restored = decode_scan(&blob, PUBLIC_SPEND).expect("it restores");
        assert_eq!(restored.outputs().len(), 0);
        assert_eq!(restored.in_flight_sends().count(), 0);

        let sends = encode_sends(&state).expect("it serializes");
        let Sends { in_flight, terminal, rings } =
            decode_sends(&sends, PUBLIC_SPEND).expect("it restores");
        assert!(in_flight.is_empty());
        assert!(terminal.is_empty());
        assert!(rings.is_empty());
    }

    #[test]
    fn a_ring_out_of_order_or_named_twice_is_refused() {
        let mut state = ScanState::new(PUBLIC_SPEND, 0, (1, 1));
        state.rings.insert([1; 32], vec![4, 9]);
        let blob = encode_sends(&state).expect("it serializes");
        let mut swapped = blob;
        let members = swapped.len().saturating_sub(16);
        swapped[members..].copy_from_slice(&[9, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
        assert_refused(
            decode_sends(&swapped, PUBLIC_SPEND),
            "a ring is not in ascending order",
            "descending members",
        );

        state.rings.insert([2; 32], vec![4, 9]);
        let mut twice = encode_sends(&state).expect("it serializes");
        let second = twice.len().saturating_sub(16 + 1 + 32);
        twice[second..second + 32].copy_from_slice(&[1; 32]);
        assert_refused(decode_sends(&twice, PUBLIC_SPEND), "it names one ring twice", "a twin");
    }
}
