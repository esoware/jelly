//! Every assertion reads the raw column through a plain connection, so it pins the disk
//! format rather than a round trip through this crate's encoder.

use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainId, Clock as _, CursorBytes, CursorScope, DerivationPath,
    KeySource, ReservationSlot, ScanAdvance, ScanStream, ScriptKind, SpkiPin, TokenId, TxDetection,
    TxDirection, TxId, TxOutcome, TxRecord,
};
use jelly_store::{NewAddress, PinRow, SyncBatch};
use rusqlite::{Connection, types::FromSql};

use crate::common::{self, Fallible};

fn column<T: FromSql>(connection: &Connection, sql: &str) -> Fallible<T> {
    Ok(connection.query_row(sql, [], |row| row.get(0))?)
}

#[tokio::test]
async fn money_is_sixteen_big_endian_bytes_and_nothing_else() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "money").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qmoney").await.unwrap();

    let record = TxRecord::builder(
        AssetId::Native(ChainId::Bitcoin),
        address.id,
        TxId::new(ChainId::Bitcoin, "moneytx"),
        TxDirection::Outgoing,
        Amount::from_base_units(u128::from(u64::MAX).saturating_add(1)),
    )
    .fee(Amount::from_base_units(1))
    .build();
    harness
        .store
        .apply_sync_batch(SyncBatch {
            txs: vec![record],
            balances: vec![BalanceUpdate {
                address_id: address.id,
                asset: AssetId::Native(ChainId::Bitcoin),
                balance: Amount::MAX,
            }],
            ..SyncBatch::default()
        })
        .await
        .unwrap();

    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();

    let amount: Vec<u8> = column(&connection, "SELECT amount FROM txs").unwrap();
    assert_eq!(hex::encode(amount), "00000000000000010000000000000000");
    let fee: Vec<u8> = column(&connection, "SELECT fee FROM txs").unwrap();
    assert_eq!(hex::encode(fee), "00000000000000000000000000000001");
    let balance: Vec<u8> = column(&connection, "SELECT amount FROM balances").unwrap();
    assert_eq!(hex::encode(balance), "ffffffffffffffffffffffffffffffff");
}

#[tokio::test]
async fn every_vocabulary_is_stored_as_the_string_the_workspace_speaks() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "spelling").await.unwrap();
    let address = harness
        .store
        .insert_address(NewAddress {
            source: KeySource::Derived { path: DerivationPath::new("m/44'/60'/0'/0/0") },
            script_kind: Some(ScriptKind::P2wpkh),
            derivation_index: Some(4),
            ..common::new_address(wallet.id, ChainId::Ethereum, "0xspelling")
        })
        .await
        .unwrap();

    let record = TxRecord::builder(
        AssetId::Token(TokenId::UsdtErc20),
        address.id,
        TxId::new(ChainId::Ethereum, "spelledtx"),
        TxDirection::SelfTransfer,
        Amount::from_base_units(1),
    )
    .outcome(TxOutcome::Unknown)
    .detection(TxDetection::BalanceDelta)
    .build();
    harness
        .store
        .apply_sync_batch(SyncBatch {
            txs: vec![record],
            scans: vec![ScanAdvance {
                scope: CursorScope::Address(address.id),
                stream: ScanStream::EvmLogScan,
                cursor: CursorBytes::copied(b"scan"),
            }],
            ..SyncBatch::default()
        })
        .await
        .unwrap();
    // A reservation lands in the same table under its own label.
    harness
        .store
        .reserve(
            CursorScope::Address(address.id),
            ReservationSlot::EvmNonces,
            CursorBytes::copied(b"nonce"),
        )
        .await
        .unwrap();
    harness
        .store
        .put_pin(PinRow {
            chain: ChainId::Ethereum,
            host: String::from("RPC.Example.COM"),
            pin: SpkiPin::new([7u8; 32], 1_700_000_000),
        })
        .await
        .unwrap();

    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();

    assert_eq!(
        column::<String>(&connection, "SELECT passphrase_policy FROM wallet_settings").unwrap(),
        "none"
    );
    assert_eq!(column::<String>(&connection, "SELECT chain FROM addresses").unwrap(), "ethereum");
    assert_eq!(
        column::<String>(&connection, "SELECT key_source FROM addresses").unwrap(),
        "derived"
    );
    assert_eq!(
        column::<String>(&connection, "SELECT derivation_path FROM addresses").unwrap(),
        "m/44'/60'/0'/0/0"
    );
    assert_eq!(
        column::<String>(&connection, "SELECT script_kind FROM addresses").unwrap(),
        "p2wpkh"
    );
    assert_eq!(column::<String>(&connection, "SELECT asset FROM txs").unwrap(), "token:usdt_erc20");
    assert_eq!(column::<String>(&connection, "SELECT direction FROM txs").unwrap(), "self");
    assert_eq!(column::<String>(&connection, "SELECT outcome FROM txs").unwrap(), "unknown");
    assert_eq!(
        column::<String>(&connection, "SELECT detection FROM txs").unwrap(),
        "balance_delta"
    );
    assert_eq!(
        column::<String>(&connection, "SELECT namespace FROM scan_cursors ORDER BY namespace")
            .unwrap(),
        "evm.log_scan"
    );
    assert_eq!(
        column::<String>(&connection, "SELECT namespace FROM scan_cursors ORDER BY namespace DESC")
            .unwrap(),
        "evm.nonces"
    );
    assert_eq!(
        column::<String>(&connection, "SELECT host FROM tls_pins").unwrap(),
        "rpc.example.com"
    );
    let pin: Vec<u8> = column(&connection, "SELECT spki_sha256 FROM tls_pins").unwrap();
    assert_eq!(pin, vec![7u8; 32]);
}

#[tokio::test]
async fn the_ordering_column_follows_the_timestamp_the_row_carries() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "ordering").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qorder").await.unwrap();
    let created_at = harness.clock.now_secs().unwrap();

    harness
        .store
        .apply_sync_batch(SyncBatch {
            txs: vec![
                common::record(address.id, "mined", Some(1_700_000_000)),
                common::record(address.id, "seen", None),
            ],
            ..SyncBatch::default()
        })
        .await
        .unwrap();

    let closed = harness.close().await.unwrap();
    let connection = Connection::open(common::db_path(&closed.directory)).unwrap();

    let mined: i64 = column(&connection, "SELECT sort_time FROM txs WHERE txid = 'mined'").unwrap();
    assert_eq!(mined, 1_700_000_000);
    let seen: i64 = column(&connection, "SELECT sort_time FROM txs WHERE txid = 'seen'").unwrap();
    assert_eq!(seen, created_at);
}
