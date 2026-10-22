use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// A currency the wallet can show values in. Every one but the dollar is a fiat
    /// pair Kraken lists against the dollar, and stored settings use these wire strings.
    pub enum FiatCurrency {
        Usd => "usd",
        Eur => "eur",
        Gbp => "gbp",
        Aud => "aud",
        Cad => "cad",
        Jpy => "jpy",
        Chf => "chf",
    }
}
