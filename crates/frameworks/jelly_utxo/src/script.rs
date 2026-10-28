use bitcoin::{Script, ScriptBuf};
use jelly_core::ScriptKind;

use crate::error::UtxoError;

/// The output script types this framework derives, weighs, and signs: the UTXO subset
/// of [`ScriptKind`], so no other chain's flavor reaches selection or signing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UtxoScript {
    P2tr,
    P2wpkh,
    P2shP2wpkh,
    P2pkh,
}

impl UtxoScript {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 4] = [Self::P2tr, Self::P2wpkh, Self::P2shP2wpkh, Self::P2pkh];

    pub(crate) const fn of(kind: ScriptKind) -> Option<Self> {
        match kind {
            ScriptKind::P2tr => Some(Self::P2tr),
            ScriptKind::P2wpkh => Some(Self::P2wpkh),
            ScriptKind::P2shP2wpkh => Some(Self::P2shP2wpkh),
            ScriptKind::P2pkh => Some(Self::P2pkh),
            ScriptKind::TonV5r1 | ScriptKind::TonV4r2 | ScriptKind::TonV3r2 => None,
        }
    }
}

impl From<UtxoScript> for ScriptKind {
    fn from(script: UtxoScript) -> Self {
        match script {
            UtxoScript::P2tr => Self::P2tr,
            UtxoScript::P2wpkh => Self::P2wpkh,
            UtxoScript::P2shP2wpkh => Self::P2shP2wpkh,
            UtxoScript::P2pkh => Self::P2pkh,
        }
    }
}

/// An output script the wallet can spend, with its kind.
///
/// The only constructor derives the kind from the script, so the two can't disagree.
/// Coin selection, weight prediction, and signing all take the kind from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpendableScript {
    script_pubkey: ScriptBuf,
    kind: UtxoScript,
}

impl SpendableScript {
    /// Fails unless the script is one of the four forms the wallet signs for.
    pub(crate) fn new(script_pubkey: ScriptBuf) -> Result<Self, UtxoError> {
        let kind = script_kind_of(&script_pubkey).ok_or(UtxoError::UnspendableScript)?;
        Ok(Self { script_pubkey, kind })
    }

    pub(crate) fn script_pubkey(&self) -> &Script {
        self.script_pubkey.as_script()
    }

    pub(crate) const fn script_buf(&self) -> &ScriptBuf {
        &self.script_pubkey
    }

    pub(crate) const fn kind(&self) -> UtxoScript {
        self.kind
    }
}

/// P2SH is read as BIP-49 wrapped segwit, the only P2SH a single-key wallet
/// derives. Any other P2SH fails at signing, because its redeem script can't be
/// recovered from the output script.
fn script_kind_of(script_pubkey: &Script) -> Option<UtxoScript> {
    if script_pubkey.is_p2tr() {
        Some(UtxoScript::P2tr)
    } else if script_pubkey.is_p2wpkh() {
        Some(UtxoScript::P2wpkh)
    } else if script_pubkey.is_p2sh() {
        Some(UtxoScript::P2shP2wpkh)
    } else if script_pubkey.is_p2pkh() {
        Some(UtxoScript::P2pkh)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::WScriptHash;
    use bitcoin::hashes::Hash as _;

    use super::*;
    use crate::test_util::{BITCOIN, test_key};

    #[test]
    fn every_script_a_fixture_address_produces_classifies_back_to_its_kind() {
        for kind in UtxoScript::ALL {
            let script = test_key(&BITCOIN, kind.into(), 1).unwrap().script_pubkey();
            assert_eq!(script_kind_of(script.as_script()), Some(kind), "{kind:?}");
            assert_eq!(SpendableScript::new(script).unwrap().kind(), kind, "{kind:?}");
        }
    }

    #[test]
    fn a_script_this_build_cannot_spend_has_no_kind() {
        let p2wsh = ScriptBuf::new_p2wsh(&WScriptHash::hash(&[0x51]));
        assert_eq!(script_kind_of(p2wsh.as_script()), None);
        assert!(matches!(SpendableScript::new(p2wsh), Err(UtxoError::UnspendableScript)));

        let op_return = ScriptBuf::new_op_return([1u8, 2, 3]);
        assert_eq!(script_kind_of(op_return.as_script()), None);
        assert_eq!(script_kind_of(ScriptBuf::new().as_script()), None);
    }
}
