//! Fixtures the unit tests share.

use jelly_core::{EndpointLabel, KeyKind, KeyMaterial};
use jelly_keypair::{PathTemplate, test_util::test_keychain};
use monero_interface::ScannableBlock;
use monero_wallet::Scanner;
use monero_wallet::block::Block;
use monero_wallet::transaction::{Pruned, Transaction};
use serde::Deserialize;

use crate::account::MoneroAccount;
use crate::entry::SPEC;
use crate::keys::MoneroKeys;
use crate::state::OwnedOutput;

/// The spend key of HD account `index` of the all-abandon phrase, at the spec's
/// default path.
pub(crate) fn hd_account(index: u32) -> KeyMaterial {
    let path = PathTemplate::parse(SPEC.default_path)
        .and_then(|template| template.path_at(index))
        .expect("a default path");
    let material =
        test_keychain().key_material_at(KeyKind::MoneroSpend, &path).expect("an account key");
    assert_eq!(material.kind(), SPEC.key_kind);
    material
}

/// A spend key whose account every test that needs one shares.
pub(crate) const SPEND_KEY: [u8; 32] = [7u8; 32];

/// The account a persisted blob is bound to, which need not be a curve point.
pub(crate) const PUBLIC_SPEND: [u8; 32] = [0x11; 32];

pub(crate) fn account() -> MoneroAccount {
    MoneroAccount::try_from(MoneroKeys::from_spend_bytes(&SPEND_KEY)).expect("the keys are usable")
}

pub(crate) fn label() -> EndpointLabel {
    EndpointLabel::new("https", "node.invalid", Some(18_081))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recorded {
    spend_key: String,
    block: String,
    pruned_transactions: Vec<String>,
    output_index_for_first_ringct_output: Option<u64>,
}

/// The outputs the recorded test-chain block pays its account, each with the key
/// image that account derives for it.
pub(crate) fn recorded_outputs() -> Vec<OwnedOutput> {
    let recorded: Recorded =
        serde_json::from_str(include_str!("../tests/fixtures/test_chain_block.json"))
            .expect("the fixture parses");
    let spend: [u8; 32] =
        hex::decode(&recorded.spend_key).expect("hex").try_into().expect("a 32-byte spend key");
    let account =
        MoneroAccount::try_from(MoneroKeys::from_spend_bytes(&spend)).expect("a usable spend key");
    let block = Block::read(&mut hex::decode(&recorded.block).expect("hex").as_slice())
        .expect("the block decodes");
    let transactions = recorded
        .pruned_transactions
        .iter()
        .map(|raw| {
            Transaction::<Pruned>::read(&mut hex::decode(raw).expect("hex").as_slice())
                .expect("the transaction decodes")
        })
        .collect();
    let scannable = ScannableBlock {
        block,
        transactions,
        output_index_for_first_ringct_output: recorded.output_index_for_first_ringct_output,
    };
    Scanner::new(account.view_pair().clone())
        .scan(scannable)
        .expect("the block scans")
        .ignore_additional_timelock()
        .into_iter()
        .map(|output| {
            let key_image = account.key_image(&output);
            OwnedOutput::new(output, 5, 0, key_image, None)
        })
        .collect()
}
