use jelly_core::{KeyKind, KeyMaterial, RawTx, SignedTx, TxId, UnsignedTx};
use monero_wallet::send::SendError;
use monero_wallet::transaction::{Input, PotentiallyPruned, Transaction};
use rand_core_06::OsRng;
use zeroize::Zeroizing;

use crate::account::MoneroAccount;
use crate::error::MoneroError;
use crate::params::{CHAIN, check_chain};
use crate::payload::UnsignedPayload;
use crate::subaddress::Subaddress;

/// A signed transaction plus what its first broadcast needs to reserve its inputs
/// and reach its ring node.
///
/// The key images are read straight off the signed transaction's inputs, not
/// derived.
pub(crate) struct Signed {
    pub tx: SignedTx,
    pub txid: [u8; 32],
    pub endpoint: String,
    pub key_images: Vec<[u8; 32]>,
    /// The height the ring was chosen against, the newest height the build knew
    /// of. The scan's own tip can be older, right after an unlock.
    pub build_height: u64,
    /// What the preview said would be paid. A Monero transaction doesn't state its
    /// own amounts, so a send that never lands is reported with this figure.
    pub amount_piconero: u64,
    pub recipient: String,
    pub subaddress: Subaddress,
}

/// Signs a built payload with the spend key behind the address it sends from,
/// without any I/O.
///
/// The ring was chosen at build time and travels inside the payload, so a payload
/// whose ring is older than the configured window is refused instead of signed
/// against a stale view of the chain.
pub(crate) fn sign(
    account: &MoneroAccount,
    tx: &UnsignedTx,
    key: &KeyMaterial,
    tip: u64,
    max_age_blocks: u64,
) -> Result<Signed, MoneroError> {
    check_chain(tx.chain())?;
    if key.kind() != KeyKind::MoneroSpend {
        return Err(MoneroError::WrongKeyKind { got: key.kind() });
    }

    let UnsignedPayload { signable, public_spend, build_height, subaddress, endpoint } =
        UnsignedPayload::decode(&tx.payload)?;
    if public_spend != account.public_spend() {
        return Err(MoneroError::PayloadForeignAccount);
    }
    let age = tip.saturating_sub(build_height);
    if age > max_age_blocks {
        return Err(MoneroError::PayloadStale { age, maximum: max_age_blocks });
    }

    let spend = key.with_bytes(|bytes| {
        if bytes.len() != 32 {
            return Err(MoneroError::WrongKeyLength { got: bytes.len() });
        }
        let mut spend = Zeroizing::new([0u8; 32]);
        spend.copy_from_slice(bytes);
        Ok(spend)
    })?;
    let scalar = account.signing_key(&spend, subaddress)?;

    let signed = signable.sign(&mut OsRng, &scalar).map_err(|error| {
        // The key already matched this account, so a wrong-key failure here means
        // the payload spends another account's outputs.
        if matches!(error, SendError::WrongPrivateKey) {
            MoneroError::PayloadForeignAccount
        } else {
            MoneroError::Sign(error)
        }
    })?;

    let txid = signed.hash();
    // `build_tx` refuses an amount above what Monero can represent, so this
    // narrowing can't lose anything. It saturates instead of carrying a second
    // error path into the signature.
    let amount_piconero = u64::try_from(tx.preview.amount.base_units()).unwrap_or(u64::MAX);
    Ok(Signed {
        tx: SignedTx {
            raw: RawTx::from_bytes(signed.serialize()),
            txid: TxId::new(CHAIN, hex::encode(txid)),
        },
        txid,
        endpoint,
        key_images: key_images_of(&signed),
        build_height,
        amount_piconero,
        recipient: tx.preview.to.clone(),
        subaddress,
    })
}

/// The key images a transaction consumes, straight out of its inputs. A miner
/// transaction's single input generates coins and has no key image.
pub(crate) fn key_images_of<P: PotentiallyPruned>(transaction: &Transaction<P>) -> Vec<[u8; 32]> {
    transaction
        .prefix()
        .inputs
        .iter()
        .filter_map(|input| match input {
            Input::ToKey { key_image, .. } => Some(key_image.to_bytes()),
            Input::Gen(_) => None,
        })
        .collect()
}

/// Reads a transaction back from the bytes that were signed.
pub(crate) fn parse_signed(raw: &[u8]) -> Result<Transaction, MoneroError> {
    let mut bytes = raw;
    let transaction = Transaction::read(&mut bytes)
        .map_err(|_| MoneroError::SignedTx { detail: "it does not decode" })?;
    if !bytes.is_empty() {
        return Err(MoneroError::SignedTx { detail: "it has trailing bytes" });
    }
    Ok(transaction)
}
