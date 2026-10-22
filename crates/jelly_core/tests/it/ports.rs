use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    ChainAdapter, ChainError, ChainId, ChainSyncProgress, Clock, CoreError, CursorBytes,
    CursorScope, CursorStore, Deadline, EventSink, KeyMaterial, PinStore, ReservationSlot,
    ScanStream, SecretText, SpkiPin, SystemClock,
    test_util::{FakeClock, MemoryCursorStore},
};

const fn assert_send_sync<T: Send + Sync>() {}

// Trait objects, errors, and secrets cross task boundaries and must be `Send + Sync`.
const _: () = assert_send_sync::<Arc<dyn Clock>>();
const _: () = assert_send_sync::<Arc<dyn EventSink>>();
const _: () = assert_send_sync::<Arc<dyn CursorStore>>();
const _: () = assert_send_sync::<Arc<dyn PinStore>>();
const _: () = assert_send_sync::<Arc<dyn ChainAdapter>>();
const _: () = assert_send_sync::<CoreError>();
const _: () = assert_send_sync::<ChainError>();
const _: () = assert_send_sync::<KeyMaterial>();
const _: () = assert_send_sync::<SecretText>();

const fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

const _: () = assert_zeroize_on_drop::<CursorBytes>();

mod clock {
    use jelly_core::duration_millis;

    use super::{Clock as _, Deadline, Duration, FakeClock, SystemClock};

    #[test]
    fn duration_milliseconds_truncate_and_saturate() {
        assert_eq!(duration_millis(Duration::ZERO), 0);
        assert_eq!(duration_millis(Duration::from_nanos(999_999)), 0);
        assert_eq!(duration_millis(Duration::from_micros(1_999)), 1);
        assert_eq!(duration_millis(Duration::from_millis(u64::MAX)), u64::MAX);
        assert_eq!(duration_millis(Duration::MAX), u64::MAX);
    }

    #[test]
    fn wall_clock_seconds_follow_the_millisecond_reading() {
        let clock = FakeClock::default();
        assert_eq!(clock.now_secs(), Ok(1_767_225_600));
        clock.advance(Duration::from_secs(90));
        assert_eq!(clock.now_secs(), Ok(1_767_225_690));
    }

    #[test]
    fn a_deadline_counts_down_on_the_monotonic_scale() {
        let clock = FakeClock::default();
        let deadline = clock.deadline_in(Duration::from_secs(30));
        assert_eq!(deadline.remaining(&clock), Duration::from_secs(30));
        assert!(!deadline.is_expired(&clock));

        clock.advance(Duration::from_secs(20));
        assert_eq!(deadline.remaining(&clock), Duration::from_secs(10));
        assert!(!deadline.is_expired(&clock));

        clock.advance(Duration::from_secs(10));
        assert_eq!(deadline.remaining(&clock), Duration::ZERO);
        assert!(deadline.is_expired(&clock));
    }

    #[test]
    fn an_expired_deadline_reports_no_time_rather_than_wrapping() {
        let clock = FakeClock::default();
        let deadline = clock.deadline_in(Duration::from_millis(1));
        clock.advance(Duration::from_secs(3600));
        assert_eq!(deadline.remaining(&clock), Duration::ZERO);
    }

    #[test]
    fn the_earlier_of_two_deadlines_is_the_one_that_binds() {
        let clock = FakeClock::default();
        let near = clock.deadline_in(Duration::from_secs(5));
        let far = clock.deadline_in(Duration::from_secs(50));
        assert_eq!(near.min(far), near);
        assert_eq!(far.min(near), near);
        assert_eq!(near.min(Deadline::NEVER), near);
        assert!(!Deadline::NEVER.is_expired(&clock));
    }

    #[test]
    fn a_budget_beyond_the_representable_range_saturates() {
        let clock = FakeClock::default();
        assert_eq!(clock.deadline_in(Duration::from_secs(u64::MAX)), Deadline::NEVER);
    }

    #[test]
    fn the_expired_deadline_has_passed_on_a_clock_that_has_not_moved() {
        let clock = FakeClock::default();
        assert_eq!(clock.elapsed_millis(), 0);
        assert!(Deadline::EXPIRED.is_expired(&clock));
        assert_eq!(Deadline::EXPIRED.remaining(&clock), Duration::ZERO);
        assert_eq!(Deadline::EXPIRED.min(Deadline::NEVER), Deadline::EXPIRED);
    }

    #[test]
    fn a_pass_is_cut_off_a_grace_past_its_deadline_even_one_already_passed() {
        let clock = FakeClock::default();
        let deadline = clock.deadline_in(Duration::from_secs(30));
        assert_eq!(
            deadline.cutoff(&clock),
            Duration::from_secs(30).saturating_add(Deadline::GRACE)
        );
        clock.advance(Duration::from_secs(3600));
        assert_eq!(deadline.cutoff(&clock), Deadline::GRACE);
        assert_eq!(Deadline::EXPIRED.cutoff(&clock), Deadline::GRACE);
    }

    #[test]
    fn the_system_clock_moves_forward_only() {
        let clock = SystemClock;
        let first = clock.elapsed_millis();
        let second = clock.elapsed_millis();
        assert!(second >= first);
        clock.now_secs().unwrap();
        clock.now_millis().unwrap();
    }
}

mod events {
    use super::{ChainId, ChainSyncProgress};

    #[test]
    fn sync_progress_is_permille_and_clamps() {
        let progress = ChainSyncProgress::new(ChainId::Monero)
            .with_progress(1500)
            .with_heights(Some(10), Some(20));
        assert_eq!(progress.progress_permille(), ChainSyncProgress::MAX_PERMILLE);
        assert_eq!(progress.synced_height(), Some(10));
        assert_eq!(progress.tip_height(), Some(20));
        assert!(progress.is_complete());
        assert!(!ChainSyncProgress::new(ChainId::Monero).is_complete());
    }

    #[test]
    fn a_fraction_rounds_down_and_never_passes_full() {
        let progress = |done, total| {
            ChainSyncProgress::new(ChainId::Bitcoin).with_fraction(done, total).progress_permille()
        };
        assert_eq!(progress(0, 3), 0);
        assert_eq!(progress(1, 3), 333, "rounded down");
        assert_eq!(progress(2, 3), 666);
        assert_eq!(progress(3, 3), ChainSyncProgress::MAX_PERMILLE);
        assert_eq!(progress(7, 3), ChainSyncProgress::MAX_PERMILLE, "more than all is all");
        assert_eq!(progress(u64::MAX - 1, u64::MAX), 999, "a huge total does not overflow");
    }

    #[test]
    fn nothing_to_do_is_done() {
        assert_eq!(ChainSyncProgress::permille_of(0, 0), ChainSyncProgress::MAX_PERMILLE);
        assert_eq!(ChainSyncProgress::permille_of(5, 0), ChainSyncProgress::MAX_PERMILLE);
    }
}

mod cursors {
    use super::{
        CursorBytes, CursorScope, CursorStore as _, MemoryCursorStore, ReservationSlot, ScanStream,
    };

    const FIRST: CursorScope = CursorScope::Address(1);
    const SECOND: CursorScope = CursorScope::Address(2);

    fn holds(cursor: Option<CursorBytes>, expected: &[u8]) -> bool {
        cursor.is_some_and(|cursor| cursor.with_bytes(|bytes| bytes == expected))
    }

    #[tokio::test]
    async fn a_memory_store_round_trips_one_cursor_per_scope_and_stream() {
        let store = MemoryCursorStore::new();
        assert!(store.is_empty());
        store
            .advance(FIRST, ScanStream::EvmLogScan, CursorBytes::copied(b"at-block-100"))
            .await
            .expect("progress");
        store
            .reserve(FIRST, ReservationSlot::EvmNonces, CursorBytes::copied(b"nonce-7"))
            .await
            .expect("a reservation");
        store
            .advance(SECOND, ScanStream::EvmLogScan, CursorBytes::copied(b"at-block-200"))
            .await
            .expect("progress");
        store
            .advance(CursorScope::Wallet, ScanStream::MoneroScan, CursorBytes::copied(b"scan"))
            .await
            .expect("progress");
        assert_eq!(store.len(), 4);
        assert_eq!(store.advance_count(), 3);
        assert_eq!(store.reserve_count(), 1);

        assert!(holds(
            store.load_scan(FIRST, ScanStream::EvmLogScan).await.expect("a read"),
            b"at-block-100"
        ));
        assert_eq!(
            store.load_scan(CursorScope::Address(3), ScanStream::EvmLogScan).await.expect("a read"),
            None
        );
        assert!(holds(
            store.load_reservation(FIRST, ReservationSlot::EvmNonces).await.expect("a read"),
            b"nonce-7"
        ));
        assert!(holds(
            store.load_scan(CursorScope::Wallet, ScanStream::MoneroScan).await.expect("a read"),
            b"scan"
        ));

        let scans = store
            .load_scans(&[FIRST, SECOND], &[ScanStream::EvmLogScan, ScanStream::MoneroScan])
            .await
            .expect("a read");
        assert_eq!(scans.len(), 2, "only the pairs that exist come back");
        let held =
            store.load_reservations(&[FIRST, SECOND], &ReservationSlot::ALL).await.expect("a read");
        assert_eq!(held.len(), 1);
    }

    #[test]
    fn only_an_address_scope_names_an_address() {
        assert_eq!(CursorScope::Address(7).address_id(), Some(7));
        assert_eq!(CursorScope::Wallet.address_id(), None);
    }
}

mod pins {
    use super::SpkiPin;

    #[test]
    fn a_pin_renders_as_its_hex_fingerprint() {
        let pin = SpkiPin::new([0xab; 32], 42);
        assert_eq!(pin.to_string(), "ab".repeat(32));
    }
}
