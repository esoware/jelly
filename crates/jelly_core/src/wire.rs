/// Declares a closed enum with one literal per variant for serde, storage, `Display`, and
/// `as_str`.
///
/// Ordering follows declaration order. Extra attributes pass through, including
/// feature-gated `specta::Type` derives. The generated constants and methods take
/// the enum's own visibility.
///
/// ```ignore
/// jelly_core::wire_enum! {
///     #[cfg_attr(feature = "ipc", derive(specta::Type))]
///     pub enum FeeLevel {
///         Slow => "slow",
///         Normal => "normal",
///     }
/// }
/// ```
#[macro_export]
macro_rules! wire_enum {
    (
        $(#[$outer:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$inner:meta])* $variant:ident => $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$outer])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            ::serde::Serialize,
            ::serde::Deserialize
        )]
        $vis enum $name {
            $(
                $(#[$inner])*
                #[serde(rename = $text)]
                $variant,
            )+
        }

        impl $name {
            $vis const COUNT: usize = [$( stringify!($variant) ),+].len();

            /// Every variant, in declaration order.
            $vis const ALL: [Self; Self::COUNT] = [$( Self::$variant ),+];

            /// The wire, storage and log spelling of this variant.
            $vis const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $text, )+
                }
            }

            /// The inverse of [`Self::as_str`].
            $vis fn parse(text: &str) -> Option<Self> {
                match text {
                    $( $text => Some(Self::$variant), )+
                    _ => None,
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
