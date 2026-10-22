use tauri::Wry;
use tauri_specta::{Builder, collect_commands, collect_events};

/// Registers the same command list that `build.rs` uses to generate permissions.
macro_rules! jelly_commands {
    ( $( $area:ident : [ $( $name:ident ),+ $(,)? ] ),+ $(,)? ) => {
        #[cfg(feature = "test-util")]
        pub const COMMAND_NAMES: &[&str] = &[ $( $( stringify!($name) ),+ ),+ ];

        fn commands(builder: Builder<Wry>) -> Builder<Wry> {
            builder.commands(collect_commands![
                $( $( crate::command::$area::$name ),+ ),+
            ])
        }
    };
}

include!("../commands.rs");

/// Registers events and records their wire names in `EVENT_NAMES`.
macro_rules! jelly_events {
    ( $( $name:ident ),+ $(,)? ) => {
        #[cfg(feature = "test-util")]
        pub const EVENT_NAMES: &[&str] =
            &[ $( <crate::event::$name as tauri_specta::Event>::NAME ),+ ];

        fn events(builder: Builder<Wry>) -> Builder<Wry> {
            builder.events(collect_events![
                $( crate::event::$name ),+
            ])
        }
    };
}

jelly_events! {
    BalanceChanged,
    TxDetected,
    TxUpdated,
    TxConfirmed,
    LockChanged,
    StartupChanged,
    PricesChanged,
    PriceHistoryChanged,
}

/// Shares command and event registration between the app and binding generation.
pub(crate) fn builder() -> Builder<Wry> {
    events(commands(Builder::new()))
        // Types serialize identically in both directions, so separate exports are
        // unnecessary.
        .disable_serde_phases()
        // IPC integers other than `Amount` and `Price` must stay below 2^53. Both use
        // decimal strings, parsed as bigint in the frontend.
        .dangerously_cast_bigints_to_number()
}
