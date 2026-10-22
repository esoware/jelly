//! Known answers.
//!
//! Each instruction layout is read back off a transaction this build signed,
//! deserialised by the ecosystem's own reader, and compared with
//! `vectors/instructions.json`.

use jelly_core::TokenId;
use jelly_solana::Method;

use crate::common::{
    self, ASSOCIATED_TOKEN_PROGRAM, Answer, COMPUTE_BUDGET_PROGRAM, Harness, OWNER, PEER,
    SYSTEM_PROGRAM, TOKEN_PROGRAM, USDC_MINT, accounts_of, instructions, request, signed,
    simulation, stock_usdc, token_account_address, usdc, vector, vector_number, with_chosen_fee,
};

/// The data bytes are compared against numbers the test chose.
///
/// The simulation is answered so the margin lands on the recorded limit, and the
/// chosen priority fee spread over that limit is the recorded price.
#[tokio::test]
async fn a_native_transfer_carries_the_pinned_layouts() {
    let harness = Harness::priced().await.unwrap();
    let units = vector_number(&["setComputeUnitLimit", "units"]).unwrap();
    let price = vector_number(&["setComputeUnitPrice", "microLamports"]).unwrap();
    let lamports = vector_number(&["systemTransfer", "lamports"]).unwrap();
    // A twenty percent margin over the consumed units is the recorded limit.
    let consumed = 37_500;
    assert_eq!(consumed * 6, units * 5);
    harness.node.always(Method::SimulateTransaction, Answer::ok(&simulation(consumed)));
    // Spread over the limit, the priority fee is the recorded price.
    let priority_lamports = 45;
    assert_eq!(priority_lamports * 1_000_000, price * units);
    let transaction =
        signed(&harness, &with_chosen_fee(PEER, common::native(), lamports, priority_lamports))
            .await
            .unwrap();

    let rows = instructions(&transaction);
    assert_eq!(rows.len(), 3, "limit, price, transfer");
    assert_eq!(
        rows.first().cloned(),
        Some((
            COMPUTE_BUDGET_PROGRAM.to_owned(),
            vector(&["setComputeUnitLimit", "data"]).unwrap()
        ))
    );
    assert_eq!(
        rows.get(1).cloned(),
        Some((
            COMPUTE_BUDGET_PROGRAM.to_owned(),
            vector(&["setComputeUnitPrice", "data"]).unwrap()
        ))
    );
    assert_eq!(
        rows.get(2).cloned(),
        Some((SYSTEM_PROGRAM.to_owned(), vector(&["systemTransfer", "data"]).unwrap()))
    );
    assert_eq!(accounts_of(&transaction, 2), vec![OWNER.to_owned(), PEER.to_owned()]);
}

#[tokio::test]
async fn a_token_transfer_carries_the_pinned_layouts_and_account_order() {
    let harness = Harness::priced().await.unwrap();
    stock_usdc(&harness.node, 50_000_000, true).unwrap();
    let amount = vector_number(&["transferChecked", "amount"]).unwrap();
    let transaction = signed(&harness, &request(PEER, usdc(), amount)).await.unwrap();

    let rows = instructions(&transaction);
    assert_eq!(rows.len(), 4, "limit, price, create, transferChecked");
    assert_eq!(
        rows.get(2).cloned(),
        Some((ASSOCIATED_TOKEN_PROGRAM.to_owned(), vector(&["createIdempotent", "data"]).unwrap()))
    );
    let (_, create) = rows.get(2).cloned().unwrap_or_default();
    assert_eq!(
        hex::decode(&create).unwrap().first().copied().map(u64::from),
        vector_number(&["createIdempotent", "tag"]),
        "CreateIdempotent"
    );
    assert_eq!(
        rows.get(3).cloned(),
        Some((TOKEN_PROGRAM.to_owned(), vector(&["transferChecked", "data"]).unwrap()))
    );

    let source = token_account_address(OWNER, TokenId::UsdcSpl).unwrap();
    let destination = token_account_address(PEER, TokenId::UsdcSpl).unwrap();
    assert_eq!(
        accounts_of(&transaction, 3),
        vec![source, USDC_MINT.to_owned(), destination.clone(), OWNER.to_owned()],
        "source, mint, destination, owner"
    );
    assert_eq!(
        accounts_of(&transaction, 2),
        vec![
            OWNER.to_owned(),
            destination,
            PEER.to_owned(),
            USDC_MINT.to_owned(),
            SYSTEM_PROGRAM.to_owned(),
            TOKEN_PROGRAM.to_owned(),
        ]
    );
}

/// The decimals the checked transfer carries are the compiled ones, which is what
/// makes the token program refuse a transfer priced against the wrong scale.
#[tokio::test]
async fn the_checked_transfer_carries_the_compiled_scale() {
    let harness = Harness::priced().await.unwrap();
    stock_usdc(&harness.node, 50_000_000, false).unwrap();
    let transaction = signed(&harness, &request(PEER, usdc(), 1_500_000)).await.unwrap();
    let rows = instructions(&transaction);
    let (_, data) = rows.get(2).cloned().unwrap_or_default();
    let bytes = hex::decode(&data).unwrap();
    assert_eq!(
        bytes.first().copied().map(u64::from),
        vector_number(&["transferChecked", "tag"]),
        "transferChecked"
    );
    assert_eq!(
        bytes.last().copied().map(u64::from),
        vector_number(&["transferChecked", "decimals"]),
        "usdc has six decimals"
    );
}

/// The compute-budget program's own numbers are read back out of the message, so
/// a chosen priority fee is spread over the measured limit the transaction asks
/// for.
#[tokio::test]
async fn a_chosen_priority_fee_reaches_the_message_over_the_measured_limit() {
    let harness = Harness::priced().await.unwrap();
    harness.node.always(Method::SimulateTransaction, Answer::ok(&simulation(10_000)));
    let transaction =
        signed(&harness, &with_chosen_fee(PEER, common::native(), 1_000, 84)).await.unwrap();
    let rows = instructions(&transaction);
    let (_, limit) = rows.first().cloned().unwrap_or_default();
    let bytes = hex::decode(&limit).unwrap();
    assert_eq!(bytes.first().copied(), Some(2), "SetComputeUnitLimit");
    assert_eq!(bytes.get(1..5).map(<[u8]>::to_vec), Some(12_000u32.to_le_bytes().to_vec()));
    let (_, price) = rows.get(1).cloned().unwrap_or_default();
    let bytes = hex::decode(&price).unwrap();
    assert_eq!(bytes.first().copied(), Some(3), "SetComputeUnitPrice");
    assert_eq!(bytes.get(1..9).map(<[u8]>::to_vec), Some(7_000u64.to_le_bytes().to_vec()));
}
