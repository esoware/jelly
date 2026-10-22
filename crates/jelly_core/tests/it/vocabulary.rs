use std::collections::BTreeSet;
use std::fmt::Debug;

use jelly_core::{
    ChainId, CustomFee, ErrorCode, FailureKind, FeeLevel, FiatCurrency, KeyKind, KeyService,
    PassphrasePolicy, PhraseWords, RejectionKind, ReservationSlot, RetryPolicy, ScanStream,
    ScriptKind, SkipReason, TokenId, Transport, TxDetection, TxDirection, TxOutcome, TxWarningKind,
};
use serde::{Serialize, de::DeserializeOwned};

use crate::common::json;

trait WireEnum: Copy + Eq + Debug + Serialize + DeserializeOwned + 'static {
    const NAME: &'static str;

    fn all() -> &'static [Self];
    fn text(self) -> &'static str;
    fn from_text(text: &str) -> Option<Self>;
}

macro_rules! wire_enums {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl WireEnum for $ty {
                const NAME: &'static str = stringify!($ty);

                fn all() -> &'static [Self] {
                    &Self::ALL
                }

                fn text(self) -> &'static str {
                    self.as_str()
                }

                fn from_text(text: &str) -> Option<Self> {
                    Self::parse(text)
                }
            }
        )+

        fn check_every_wire_enum() {
            $( check::<$ty>(); )+
        }
    };
}

wire_enums![
    ChainId,
    ScriptKind,
    TokenId,
    TxDirection,
    TxOutcome,
    TxDetection,
    TxWarningKind,
    FeeLevel,
    CustomFee,
    RejectionKind,
    RetryPolicy,
    FailureKind,
    ErrorCode,
    KeyKind,
    PassphrasePolicy,
    PhraseWords,
    ScanStream,
    ReservationSlot,
    FiatCurrency,
    Transport,
    KeyService,
    SkipReason,
];

#[expect(
    clippy::expect_used,
    reason = "a fixture helper outside a #[test] body is still test code and must fail loudly"
)]
fn check<T: WireEnum>() {
    let all = T::all();
    assert!(!all.is_empty(), "{} has no variants", T::NAME);

    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for &variant in all {
        let text = variant.text();
        assert!(!text.is_empty(), "{}::{variant:?} has an empty string", T::NAME);
        assert!(
            text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._".contains(c)),
            "{}::{variant:?} spells itself {text}",
            T::NAME
        );
        assert!(seen.insert(text), "{} spells two variants {text}", T::NAME);

        assert_eq!(
            T::from_text(text),
            Some(variant),
            "{}::{variant:?} does not round trip",
            T::NAME
        );
        assert_eq!(
            json(&variant),
            format!("\"{text}\""),
            "{}::{variant:?} serializes elsewhere",
            T::NAME
        );

        let back: T = serde_json::from_str(&json(&variant)).expect("deserialize");
        assert_eq!(back, variant, "{}::{variant:?} does not survive serde", T::NAME);
    }

    assert_eq!(T::from_text("not-a-variant-of-anything"), None, "{} accepts junk", T::NAME);
    assert_eq!(T::from_text(""), None, "{} accepts the empty string", T::NAME);
}

#[test]
fn every_closed_enum_keeps_its_contract() {
    check_every_wire_enum();
}

mod declaration {
    use crate::common::json;

    jelly_core::wire_enum! {
        pub(crate) enum Sample {
            First => "first",
            Second => "second",
        }
    }

    #[test]
    fn a_crate_visible_enum_expands_and_round_trips_through_its_wire_strings() {
        assert_eq!(Sample::COUNT, 2);
        assert_eq!(Sample::ALL, [Sample::First, Sample::Second]);
        for variant in Sample::ALL {
            assert_eq!(Sample::parse(variant.as_str()), Some(variant));
            assert_eq!(variant.to_string(), variant.as_str());
            let text = json(&variant);
            assert_eq!(text, format!("\"{}\"", variant.as_str()));
            assert_eq!(serde_json::from_str::<Sample>(&text).expect("it deserializes"), variant);
        }
        assert_eq!(Sample::parse("third"), None);
        assert!(Sample::First < Sample::Second, "ordering follows declaration order");
    }
}

mod chains {
    use super::{ChainId, ScriptKind};

    /// The table lists every chain in declaration order, so a new chain fails to compile
    /// here until it has a row.
    #[test]
    fn native_decimals_match_each_chains_smallest_unit() {
        let expected = [
            (ChainId::Bitcoin, 8),
            (ChainId::Litecoin, 8),
            (ChainId::Dogecoin, 8),
            (ChainId::BitcoinCash, 8),
            (ChainId::Zcash, 8),
            (ChainId::Ethereum, 18),
            (ChainId::Bsc, 18),
            (ChainId::Polygon, 18),
            (ChainId::Arbitrum, 18),
            (ChainId::Base, 18),
            (ChainId::Avalanche, 18),
            (ChainId::Solana, 9),
            (ChainId::Tron, 6),
            (ChainId::Xrp, 6),
            (ChainId::Ton, 9),
            (ChainId::Monero, 12),
        ];
        assert_eq!(expected.map(|(chain, _)| chain), ChainId::ALL);
        for (chain, decimals) in expected {
            assert_eq!(chain.native_decimals(), decimals, "{}", chain.as_str());
        }
    }

    #[test]
    fn script_kinds_carry_their_bip44_purpose() {
        assert_eq!(ScriptKind::P2tr.purpose(), Some(86));
        assert_eq!(ScriptKind::P2wpkh.purpose(), Some(84));
        assert_eq!(ScriptKind::P2shP2wpkh.purpose(), Some(49));
        assert_eq!(ScriptKind::P2pkh.purpose(), Some(44));
    }

    #[test]
    fn a_ton_wallet_version_derives_under_no_purpose_of_its_own() {
        for kind in [ScriptKind::TonV5r1, ScriptKind::TonV4r2, ScriptKind::TonV3r2] {
            assert_eq!(kind.purpose(), None, "{kind}");
        }
    }
}

mod cursors {
    use super::{ReservationSlot, ScanStream};

    #[test]
    fn every_label_names_a_family_and_a_stream() {
        for stream in ScanStream::ALL {
            assert_eq!(stream.as_str().matches('.').count(), 1, "{}", stream.as_str());
        }
        for slot in ReservationSlot::ALL {
            assert_eq!(slot.as_str().matches('.').count(), 1, "{}", slot.as_str());
        }
    }

    /// Stream and reservation names share one column.
    #[test]
    fn scan_streams_and_reservation_slots_share_no_label() {
        for stream in ScanStream::ALL {
            assert!(ReservationSlot::parse(stream.as_str()).is_none(), "{stream}");
        }
        for slot in ReservationSlot::ALL {
            assert!(ScanStream::parse(slot.as_str()).is_none(), "{slot}");
        }
    }
}
