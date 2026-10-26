use bitcoin::{OutPoint, Script};
use jelly_core::{Amount, AssetId, FeeRate, TxDetail, TxPreview, TxWarning, TxWarningKind};

use crate::coin::value_of;
use crate::error::UtxoError;
use crate::fee::rate_for;
use crate::params::UtxoParams;
use crate::payload::UtxoPayload;
use crate::weight::vsize;

/// A rate more than this many times the fast level's gets a warning.
const HIGH_RATE_MULTIPLE: u64 = 20;
/// A rate under `1 / LOW_RATE_DIVISOR` of the network's slow rate gets a warning,
/// because a rate just under it still confirms.
const LOW_RATE_DIVISOR: u64 = 2;

/// The fee context the preview warnings compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewQuote {
    pub(crate) relay_floor: FeeRate,
    /// The fast level's rate.
    pub(crate) fast: FeeRate,
    /// What the network takes for a slow send, which unlike the slow level is not
    /// raised to the wallet minimum.
    pub(crate) slow_market: FeeRate,
    /// Later mempool transactions a replacement cancels along with the original.
    pub(crate) evicted: usize,
}

#[derive(Debug, Clone)]
struct PreviewInput {
    outpoint: OutPoint,
    value: Amount,
    address: Option<String>,
}

#[derive(Debug, Clone)]
struct PreviewOutput<'a> {
    script_pubkey: &'a Script,
    address: Option<String>,
    value: Amount,
    is_change: bool,
}

/// Everything the preview knows, before it is flattened into the rows the
/// interface renders.
#[derive(Debug, Clone)]
struct DecodedTx<'a> {
    inputs: Vec<PreviewInput>,
    outputs: Vec<PreviewOutput<'a>>,
    /// What the recorded recipient receives.
    paid: Amount,
    fee: Amount,
    vsize: u64,
    /// The rate this transaction really pays, not the one that was asked for.
    rate: FeeRate,
}

/// Everything comes from the payload the signer will consume, not from the
/// request that produced it.
fn decode<'a>(params: &UtxoParams, payload: &'a UtxoPayload) -> Result<DecodedTx<'a>, UtxoError> {
    let intent = payload.intent();
    let change_address = intent.sources.first().ok_or(UtxoError::payload("the sending address"))?;
    let change_script_pubkey = params.script_pubkey(change_address)?;
    let recipient_script_pubkey = params.script_pubkey(&intent.to)?;
    let tx = payload.unsigned_tx();

    let inputs: Vec<PreviewInput> = payload
        .inputs()
        .iter()
        .map(|input| PreviewInput {
            outpoint: input.outpoint,
            value: input.value,
            address: params.address_of(input.script.script_pubkey()),
        })
        .collect();

    // An output paying the first source is change unless it is the recorded
    // payment, which is found by the recipient the payload names. A child that only
    // pays for its parent makes no payment, so all it pays back is change.
    let recipient_index = if intent.pays_for.is_some() {
        None
    } else {
        Some(
            tx.output
                .iter()
                .position(|txout| txout.script_pubkey == recipient_script_pubkey)
                .ok_or(UtxoError::RecipientNotFound)?,
        )
    };
    let outputs: Vec<PreviewOutput<'_>> = tx
        .output
        .iter()
        .enumerate()
        .map(|(index, txout)| PreviewOutput {
            script_pubkey: txout.script_pubkey.as_script(),
            address: params.address_of(txout.script_pubkey.as_script()),
            value: value_of(txout),
            is_change: Some(index) != recipient_index
                && txout.script_pubkey == change_script_pubkey,
        })
        .collect();

    let paid = recipient_index
        .and_then(|index| outputs.get(index))
        .map_or(Amount::ZERO, |output| output.value);
    let weight = payload.predicted_weight();

    Ok(DecodedTx {
        inputs,
        outputs,
        paid,
        fee: payload.fee(),
        vsize: vsize(weight),
        rate: rate_for(payload.fee(), weight)?,
    })
}

pub(crate) fn preview(
    params: &UtxoParams,
    payload: &UtxoPayload,
    quote: PreviewQuote,
) -> Result<TxPreview, UtxoError> {
    let decoded = decode(params, payload)?;
    let warnings = warnings(params, payload, &decoded, quote);
    Ok(flatten(params, payload, &decoded, warnings))
}

/// The warnings the interface must render before the user signs.
fn warnings(
    params: &UtxoParams,
    payload: &UtxoPayload,
    decoded: &DecodedTx<'_>,
    quote: PreviewQuote,
) -> Vec<TxWarning> {
    let mut warnings = Vec::new();
    let intent = payload.intent();

    for output in &decoded.outputs {
        let threshold = params.dust_threshold(output.script_pubkey);
        if output.value < threshold {
            let to = output.address.as_deref().unwrap_or("a non-standard script");
            warnings.push(TxWarning::new(
                TxWarningKind::DustOutput,
                format!(
                    "The output of {} to {to} is below the {} network minimum. Nodes will reject it.",
                    params.chain.format_amount(output.value),
                    params.chain.format_amount(threshold)
                ),
            ));
        }
    }

    if !intent.donated.is_zero() {
        warnings.push(TxWarning::new(
            TxWarningKind::DroppedChange,
            format!(
                "The remaining {} would cost more to return as change. It is added to the fee.",
                params.chain.format_amount(intent.donated)
            ),
        ));
    }

    for note in params.address_notes {
        if intent.to.starts_with(note.prefix) {
            warnings.push(TxWarning::new(note.kind, note.message));
        }
    }

    if let Some(parent) = intent.pays_for {
        warnings.push(TxWarning::new(
            TxWarningKind::ChildPays,
            format!(
                "This sends a second transaction that pays the fee for {parent}, which stays as it is, and for any earlier unconfirmed transaction it waits on. Nothing leaves your wallet but this fee."
            ),
        ));
    }

    warnings.extend(rate_warning(params, decoded, quote, intent.pays_for.is_some()));

    if let Some(replaced) = intent.replaces {
        warnings.push(TxWarning::new(
            TxWarningKind::Replacement,
            format!("This replaces {}, which is still unconfirmed.", replaced.txid),
        ));
        // A child spending the original's outputs cannot outlive it, and Bitcoin
        // Core refuses such a bump outright rather than drop the child silently.
        match quote.evicted {
            0 => {}
            1 => warnings.push(TxWarning::new(
                TxWarningKind::Replacement,
                "This also cancels a later unconfirmed transaction that spends its outputs. Send that payment again once this one confirms.",
            )),
            count => warnings.push(TxWarning::new(
                TxWarningKind::Replacement,
                format!(
                    "This also cancels {count} later unconfirmed transactions that spend its outputs. Send those payments again once this one confirms."
                ),
            )),
        }
        if let Some(reduction) = replaced.reduced_payment {
            warnings.push(TxWarning::new(
                TxWarningKind::Replacement,
                format!(
                    "The higher fee reduces the payment to {} instead of {}.",
                    params.chain.format_amount(reduction.to),
                    params.chain.format_amount(reduction.from)
                ),
            ));
        }
    }

    warnings
}

/// A warning about the rate itself, when it is below what the network relays or far
/// from what it currently takes.
///
/// There is no warning kind for a low rate, so it uses `Other` rather than `HighFee`.
/// A child's own rate is high by design, because it pays for others, so only the relay
/// floor is checked for one.
fn rate_warning(
    params: &UtxoParams,
    decoded: &DecodedTx<'_>,
    quote: PreviewQuote,
    is_child: bool,
) -> Option<TxWarning> {
    let rate = params.format_rate(decoded.rate);
    if decoded.rate < quote.relay_floor {
        return Some(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "The {rate} fee rate is below the {} network minimum. Most nodes will reject it.",
                params.format_rate(quote.relay_floor)
            ),
        ));
    }
    if is_child {
        return None;
    }
    if decoded.rate.sat_per_kwu() < quote.slow_market.sat_per_kwu().div_euclid(LOW_RATE_DIVISOR) {
        return Some(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "The {rate} fee rate is under half the network's slow rate of {}. Confirmation may take a long time.",
                params.format_rate(quote.slow_market)
            ),
        ));
    }
    (decoded.rate.sat_per_kwu() > quote.fast.sat_per_kwu().saturating_mul(HIGH_RATE_MULTIPLE)).then(
        || {
            TxWarning::new(
                TxWarningKind::HighFee,
                format!(
                    "The {rate} fee rate is far above the fast rate of {}. Check the fee before signing.",
                    params.format_rate(quote.fast)
                ),
            )
        },
    )
}

fn flatten(
    params: &UtxoParams,
    payload: &UtxoPayload,
    decoded: &DecodedTx<'_>,
    warnings: Vec<TxWarning>,
) -> TxPreview {
    let intent = payload.intent();
    let mut details: Vec<TxDetail> = Vec::new();
    if let Some(replaced) = intent.replaces {
        details.push(TxDetail::new("Replaces", replaced.txid.to_string()));
    }
    if let Some(parent) = intent.pays_for {
        details.push(TxDetail::new("Pays for", parent.to_string()));
    }
    details.push(TxDetail::new("Inputs", decoded.inputs.len().to_string()));
    for (index, input) in decoded.inputs.iter().enumerate() {
        let from =
            input.address.as_deref().map_or_else(String::new, |address| format!(" from {address}"));
        details.push(TxDetail::new(
            format!("Input {index}"),
            format!("{} - {}{from}", input.outpoint, params.chain.format_amount(input.value)),
        ));
    }
    details.push(TxDetail::new("Outputs", decoded.outputs.len().to_string()));
    for (index, output) in decoded.outputs.iter().enumerate() {
        let note = if output.is_change { ", change" } else { "" };
        let to = output.address.as_deref().unwrap_or("a non-standard script");
        details.push(TxDetail::new(
            format!("Output {index}"),
            format!("{} to {to}{note}", params.chain.format_amount(output.value)),
        ));
    }
    details.push(TxDetail::new("Fee", params.chain.format_amount(decoded.fee)));
    details.push(TxDetail::new("Fee rate", params.format_rate(decoded.rate)));
    if params.has_segwit {
        details.push(TxDetail::new("Virtual size", format!("{} vB", decoded.vsize)));
    } else {
        details.push(TxDetail::new("Size", format!("{} bytes", decoded.vsize)));
    }
    if !intent.donated.is_zero() {
        details.push(TxDetail::new("To the miner", params.chain.format_amount(intent.donated)));
    }

    TxPreview {
        chain: params.chain,
        asset: AssetId::Native(params.chain),
        from: intent.sources.clone(),
        to: intent.to.clone(),
        destination_tag: None,
        amount: decoded.paid,
        fee: decoded.fee,
        details,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::base58;

    use super::*;
    use crate::build::build_tx;
    use crate::coin::units;
    use crate::fee::change_floor;
    use crate::fixtures::{
        FLOOR_QUOTE, ONE_PER_VB, TEN_PER_VB, TIP, bitcoin_cash_scene, bitcoin_scene, litecoin_scene,
    };
    use crate::payload::FIELD_TO;
    use crate::test_util::test_key;

    fn kinds(warnings: &[TxWarning]) -> Vec<TxWarningKind> {
        warnings.iter().map(|warning| warning.kind).collect()
    }

    #[test]
    fn change_back_to_the_spending_address_is_labelled_as_change() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let decoded = decode(&scene.params, &built.payload).unwrap();

        assert_eq!(decoded.outputs.len(), 2);
        let payment = decoded.outputs.first().expect("a payment");
        let change = decoded.outputs.get(1).expect("change");
        assert!(!payment.is_change);
        assert!(change.is_change);
        assert_eq!(payment.address.as_deref(), Some(scene.theirs.address()));
        assert_eq!(change.address.as_deref(), Some(scene.ours.address()));
        assert_eq!(decoded.paid, units(50_000));
        let input_total = Amount::checked_sum(decoded.inputs.iter().map(|input| input.value));
        let output_total = Amount::checked_sum(decoded.outputs.iter().map(|output| output.value));
        assert_eq!(input_total, output_total.and_then(|total| total.checked_add(decoded.fee)));
    }

    #[test]
    fn paying_yourself_does_not_turn_the_payment_into_change() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[200_000]);
        let mut request = scene.request(Some(50_000), ONE_PER_VB, &coins);
        request.to = scene.ours.address();
        request.to_script = scene.ours.script().script_pubkey();
        let built = build_tx(&scene.params, &request).unwrap();
        let decoded = decode(&scene.params, &built.payload).unwrap();

        assert!(!decoded.outputs.first().expect("a payment").is_change);
        assert!(decoded.outputs.get(1).is_none_or(|change| change.is_change));
        assert_eq!(decoded.paid, units(50_000));
    }

    #[test]
    fn the_displayed_rate_is_the_real_one() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), TEN_PER_VB, &scene.coins(&[200_000])).unwrap();
        let decoded = decode(&scene.params, &built.payload).unwrap();
        let weight = built.payload.predicted_weight();
        let tx = built.payload.unsigned_tx();

        assert_eq!(weight, built.selection.weight);
        assert_eq!(decoded.vsize, 141);
        assert_eq!(decoded.fee, built.selection.fee);
        assert_eq!(decoded.rate, rate_for(decoded.fee, weight).unwrap());
        assert!(decoded.rate >= TEN_PER_VB, "{:?} must not undershoot", decoded.rate);
        assert!(tx.input.iter().any(|txin| txin.sequence.is_rbf()));
        assert_eq!(tx.lock_time.to_consensus_u32(), TIP);
        assert_eq!(
            tx.input.iter().map(|txin| txin.sequence).collect::<Vec<_>>(),
            vec![scene.params.replacement.input_sequence()]
        );
    }

    #[test]
    fn every_input_and_output_renders_as_an_address_on_both_chains() {
        for scene in [bitcoin_scene().unwrap(), litecoin_scene().unwrap()] {
            let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
            let decoded = decode(&scene.params, &built.payload).unwrap();
            assert!(decoded.inputs.iter().all(|input| input.address.is_some()));
            assert!(decoded.outputs.iter().all(|output| output.address.is_some()));
            assert_eq!(
                decoded.inputs.first().and_then(|input| input.address.clone()),
                Some(scene.ours.address().to_owned())
            );
        }
    }

    #[test]
    fn a_dust_output_is_warned_about_with_the_chains_own_threshold() {
        let scene = litecoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let mut dusted = built.payload.psbt().clone();
        // Above Bitcoin's 294 and below Litecoin's 2940.
        if let Some(output) = dusted.unsigned_tx.output.first_mut() {
            output.value = bitcoin::Amount::from_sat(1_000);
        }
        let payload = UtxoPayload::decode(&dusted.serialize()).unwrap();
        let rendered = preview(&scene.params, &payload, FLOOR_QUOTE).unwrap();
        let warning = rendered
            .warnings
            .iter()
            .find(|warning| warning.kind == TxWarningKind::DustOutput)
            .unwrap_or_else(|| panic!("{:?}", rendered.warnings));
        assert!(warning.message.contains("below the 0.0000294 LTC network minimum"), "{warning:?}");
    }

    #[test]
    fn value_that_went_to_the_miner_is_reported() {
        let scene = bitcoin_scene().unwrap();
        let floor = change_floor(&scene.params, ONE_PER_VB, scene.ours.script()).unwrap();
        let leftover = u64::try_from(floor.base_units()).unwrap() - 1;
        let built = scene
            .build(Some(50_000), ONE_PER_VB, &scene.coins(&[50_000 + 110 + leftover]))
            .unwrap();
        assert_eq!(built.selection.donated, units(leftover));

        let rendered = preview(&scene.params, &built.payload, FLOOR_QUOTE).unwrap();
        assert!(
            kinds(&rendered.warnings).contains(&TxWarningKind::DroppedChange),
            "{:?}",
            rendered.warnings
        );
        assert!(
            rendered.details.iter().any(|detail| detail.label == "To the miner"),
            "{:?}",
            rendered.details
        );
    }

    #[test]
    fn litecoins_bitcoin_shaped_legacy_address_is_flagged_and_bitcoins_is_not() {
        let ltc = litecoin_scene().unwrap();
        let wrapped = test_key(&ltc.params, jelly_core::ScriptKind::P2shP2wpkh, 5).unwrap();
        // Litecoin writes P2SH under its own version byte, so a legacy `3...` address
        // has to be constructed rather than derived.
        let legacy = {
            let mut payload = vec![0x05u8];
            payload.extend_from_slice(
                wrapped.script().script_pubkey().as_bytes().get(2..22).expect("a hash"),
            );
            base58::encode_check(&payload)
        };
        assert!(legacy.starts_with('3'), "{legacy}");

        // Both chains read the `3...` form as the same script hash.
        let legacy_script = ltc.params.script_pubkey(&legacy).unwrap();
        let coins = ltc.coins(&[500_000]);
        let mut request = ltc.request(Some(50_000), ONE_PER_VB, &coins);
        request.to = &legacy;
        request.to_script = &legacy_script;
        let built = build_tx(&ltc.params, &request).unwrap();
        let rendered = preview(&ltc.params, &built.payload, FLOOR_QUOTE).unwrap();
        assert!(
            kinds(&rendered.warnings).contains(&TxWarningKind::AmbiguousAddress),
            "{:?}",
            rendered.warnings
        );

        let btc = bitcoin_scene().unwrap();
        let coins = btc.coins(&[500_000]);
        let mut request = btc.request(Some(50_000), ONE_PER_VB, &coins);
        request.to = &legacy;
        request.to_script = &legacy_script;
        let built = build_tx(&btc.params, &request).unwrap();
        let rendered = preview(&btc.params, &built.payload, FLOOR_QUOTE).unwrap();
        assert!(!kinds(&rendered.warnings).contains(&TxWarningKind::AmbiguousAddress));
    }

    #[test]
    fn a_rate_well_below_the_network_rate_and_one_far_above_it_are_both_flagged() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();

        let busy = PreviewQuote { fast: TEN_PER_VB, slow_market: TEN_PER_VB, ..FLOOR_QUOTE };
        let rendered = preview(&scene.params, &built.payload, busy).unwrap();
        assert!(
            kinds(&rendered.warnings).contains(&TxWarningKind::Other),
            "{:?}",
            rendered.warnings
        );
        assert!(
            rendered.warnings.iter().any(|warning| warning.message.contains("under half")),
            "{:?}",
            rendered.warnings
        );

        let generous = scene
            .build(Some(50_000), FeeRate::from_sat_per_kwu(25_000), &scene.coins(&[200_000]))
            .unwrap();
        let rendered = preview(&scene.params, &generous.payload, FLOOR_QUOTE).unwrap();
        assert!(
            rendered.warnings.iter().any(|warning| warning.kind == TxWarningKind::HighFee
                && warning.message.contains("far above the fast rate")),
            "{:?}",
            rendered.warnings
        );
    }

    /// Litecoin's levels pay its 10 lit/vB wallet minimum even when 2 lit/vB confirms
    /// in the next block, and a rate just under the network's still confirms.
    #[test]
    fn a_relayable_rate_near_the_network_rate_is_not_called_slow() {
        let ltc = litecoin_scene().unwrap();
        let two = FeeRate::from_sat_per_kwu(500);
        let built = ltc.build(Some(50_000), two, &ltc.coins(&[500_000])).unwrap();
        let quiet = PreviewQuote { fast: TEN_PER_VB, slow_market: two, ..FLOOR_QUOTE };
        let rendered = preview(&ltc.params, &built.payload, quiet).unwrap();
        assert!(
            !kinds(&rendered.warnings).contains(&TxWarningKind::Other),
            "{:?}",
            rendered.warnings
        );

        let btc = bitcoin_scene().unwrap();
        let built = btc.build(Some(50_000), TEN_PER_VB, &btc.coins(&[200_000])).unwrap();
        let just_above = PreviewQuote {
            fast: FeeRate::from_sat_per_kwu(2_501),
            slow_market: FeeRate::from_sat_per_kwu(2_501),
            ..FLOOR_QUOTE
        };
        let rendered = preview(&btc.params, &built.payload, just_above).unwrap();
        assert!(
            !kinds(&rendered.warnings).contains(&TxWarningKind::Other),
            "{:?}",
            rendered.warnings
        );
    }

    #[test]
    fn a_rate_below_the_relay_floor_is_flagged_as_unrelayable() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let raised = PreviewQuote {
            relay_floor: TEN_PER_VB,
            fast: TEN_PER_VB,
            slow_market: TEN_PER_VB,
            evicted: 0,
        };
        let rendered = preview(&scene.params, &built.payload, raised).unwrap();
        assert!(
            rendered.warnings.iter().any(|warning| warning.message.contains("network minimum")),
            "{:?}",
            rendered.warnings
        );
        assert!(
            !kinds(&rendered.warnings).contains(&TxWarningKind::HighFee),
            "an unrelayably low fee is not a high one: {:?}",
            rendered.warnings
        );
    }

    #[test]
    fn a_rate_warning_counts_in_the_chains_own_unit() {
        let scene = litecoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[500_000])).unwrap();
        let raised = PreviewQuote {
            relay_floor: TEN_PER_VB,
            fast: TEN_PER_VB,
            slow_market: TEN_PER_VB,
            evicted: 0,
        };
        let rendered = preview(&scene.params, &built.payload, raised).unwrap();
        let warning = rendered
            .warnings
            .iter()
            .find(|warning| warning.message.contains("network minimum"))
            .expect("the rate is below the floor");
        assert!(warning.message.contains("below the 10 lit/vB network minimum"), "{warning:?}");
        assert!(!warning.message.contains("sat/vB"), "{warning:?}");
    }

    #[test]
    fn the_flattened_preview_carries_every_row_the_interface_renders() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let rendered = preview(&scene.params, &built.payload, FLOOR_QUOTE).unwrap();

        assert_eq!(rendered.chain, scene.params.chain);
        assert_eq!(rendered.asset, AssetId::Native(scene.params.chain));
        assert_eq!(rendered.from, [scene.ours.address()]);
        assert_eq!(rendered.to, scene.theirs.address());
        assert_eq!(rendered.amount, units(50_000));
        assert_eq!(rendered.fee, built.selection.fee);

        let labels: Vec<&str> =
            rendered.details.iter().map(|detail| detail.label.as_str()).collect();
        for wanted in ["Inputs", "Outputs", "Fee", "Fee rate", "Virtual size"] {
            assert!(labels.contains(&wanted), "{wanted} is missing from {labels:?}");
        }
    }

    /// Without segregated witness a virtual byte is a byte, so the row says so.
    #[test]
    fn a_chain_without_segwit_shows_the_size_in_bytes() {
        let scene = bitcoin_cash_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let rendered = preview(&scene.params, &built.payload, FLOOR_QUOTE).unwrap();
        let size = rendered.details.iter().find(|detail| detail.label == "Size").unwrap();
        assert_eq!(size.value, "226 bytes");
        assert!(rendered.details.iter().all(|detail| detail.label != "Virtual size"));
    }

    #[test]
    fn a_payload_whose_recipient_is_not_paid_is_refused() {
        let scene = bitcoin_scene().unwrap();
        let stranger = test_key(&scene.params, jelly_core::ScriptKind::P2wpkh, 9).unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let mut moved = built.payload.psbt().clone();
        let key = moved
            .proprietary
            .keys()
            .find(|key| key.subtype == FIELD_TO)
            .cloned()
            .expect("the recipient field");
        moved.proprietary.insert(key, stranger.address().as_bytes().to_vec());
        let payload = UtxoPayload::decode(&moved.serialize()).unwrap();
        assert!(matches!(decode(&scene.params, &payload), Err(UtxoError::RecipientNotFound)));
    }

    #[test]
    fn a_send_max_previews_the_whole_balance_less_the_fee() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(None, ONE_PER_VB, &scene.coins(&[70_000, 30_000])).unwrap();
        let rendered = preview(&scene.params, &built.payload, FLOOR_QUOTE).unwrap();
        assert_eq!(rendered.amount.checked_add(rendered.fee), Some(units(100_000)));
        assert_eq!(rendered.amount, built.selection.recipient_value);
        assert!(rendered.amount > Amount::ZERO);
    }
}
