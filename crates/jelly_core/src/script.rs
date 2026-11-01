use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// The flavors one key's address can take: a UTXO output script type, or the TON
    /// wallet contract that holds the key. The same private key produces a different
    /// address for each.
    pub enum ScriptKind {
        /// BIP-86 taproot.
        P2tr => "p2tr",
        /// BIP-84 native segwit.
        P2wpkh => "p2wpkh",
        /// BIP-49 wrapped segwit.
        P2shP2wpkh => "p2sh_p2wpkh",
        /// BIP-44 legacy.
        P2pkh => "p2pkh",
        TonV5r1 => "ton_v5r1",
        TonV4r2 => "ton_v4r2",
        TonV3r2 => "ton_v3r2",
    }
}

impl ScriptKind {
    /// The BIP-44 purpose this flavor derives under. A TON wallet contract derives under
    /// one path whatever its version, so it has none.
    pub const fn purpose(self) -> Option<u32> {
        match self {
            Self::P2tr => Some(86),
            Self::P2wpkh => Some(84),
            Self::P2shP2wpkh => Some(49),
            Self::P2pkh => Some(44),
            Self::TonV5r1 | Self::TonV4r2 | Self::TonV3r2 => None,
        }
    }
}
