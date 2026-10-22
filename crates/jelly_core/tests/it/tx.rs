use jelly_core::{
    Amount, AssetId, ChainId, RawTx, SignedTx, TxDetection, TxDirection, TxId, TxOutcome,
    TxPayload, TxPreview, TxRecord, UnsignedTx, confirmations,
};

const TIP: u64 = 800_000;

mod formula {
    use super::{TIP, confirmations};

    #[test]
    fn an_unmined_transaction_has_no_confirmations() {
        assert_eq!(confirmations(None, TIP), 0);
        assert_eq!(confirmations(None, 0), 0);
    }

    #[test]
    fn a_transaction_at_the_tip_has_exactly_one_confirmation() {
        assert_eq!(confirmations(Some(TIP), TIP), 1);
        assert_eq!(confirmations(Some(0), 0), 1);
    }

    #[test]
    fn depth_counts_the_mining_block_itself() {
        assert_eq!(confirmations(Some(799_999), TIP), 2);
        assert_eq!(confirmations(Some(799_995), TIP), 6);
        assert_eq!(confirmations(Some(0), 5), 6);
    }

    #[test]
    fn a_height_above_the_cached_tip_counts_as_one_confirmation() {
        assert_eq!(confirmations(Some(800_001), TIP), 1);
        assert_eq!(confirmations(Some(u64::MAX), TIP), 1);
    }

    #[test]
    fn an_unreachable_depth_saturates_instead_of_wrapping() {
        assert_eq!(confirmations(Some(0), u64::MAX), u32::MAX);
    }
}

mod builder {
    use super::{
        Amount, AssetId, ChainId, TIP, TxDetection, TxDirection, TxId, TxOutcome, TxRecord,
        confirmations,
    };

    #[test]
    fn a_new_record_is_pending_successful_and_fully_decoded() {
        let record = TxRecord::builder(
            AssetId::Native(ChainId::Bitcoin),
            42,
            TxId::new(ChainId::Bitcoin, "d4f1"),
            TxDirection::Incoming,
            Amount::from_base_units(1_000),
        )
        .build();
        assert_eq!(record.txid, TxId::new(ChainId::Bitcoin, "d4f1"));
        assert_eq!(record.address_id, 42);
        assert_eq!(record.asset, AssetId::Native(ChainId::Bitcoin));
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.amount, Amount::from_base_units(1_000));
        assert_eq!(record.outcome, TxOutcome::Success);
        assert_eq!(record.detection, TxDetection::Full);
        assert_eq!(record.confirmations, 0);
        assert_eq!(record.height, None);
        assert_eq!(record.timestamp, None);
        assert_eq!(record.fee, None);
        assert_eq!(record.counterparty, None);
    }

    #[test]
    fn confirmed_at_derives_the_count_from_the_tip_in_the_same_step() {
        let record = TxRecord::builder(
            AssetId::Native(ChainId::Litecoin),
            1,
            TxId::new(ChainId::Litecoin, "aa"),
            TxDirection::Outgoing,
            Amount::from_base_units(5),
        )
        .confirmed_at(TIP, Some(1_700_000_000), TIP)
        .build();
        assert_eq!(record.height, Some(TIP));
        assert_eq!(record.timestamp, Some(1_700_000_000));
        assert_eq!(record.confirmations, 1);
        assert_eq!(record.confirmations, confirmations(record.height, TIP));
    }
}

mod opaque_bytes {
    use super::{RawTx, TxPayload};

    #[test]
    fn a_payload_reports_its_length_and_not_its_bytes() {
        let payload = TxPayload::from_bytes(vec![1, 2, 3, 4]);
        assert_eq!(payload.as_bytes(), &[1, 2, 3, 4]);
        assert_eq!(format!("{payload:?}"), "TxPayload(4 bytes)");
    }

    #[test]
    fn raw_bytes_report_their_length_and_not_their_contents() {
        let raw = RawTx::from_bytes(vec![0xde, 0xad]);
        assert_eq!(raw.as_bytes(), &[0xde, 0xad]);
        assert_eq!(format!("{raw:?}"), "RawTx(2 bytes)");
        assert_eq!(format!("{:?}", RawTx::from_bytes(Vec::new())), "RawTx(0 bytes)");
    }
}

mod one_chain_per_transaction {
    use super::{
        Amount, AssetId, ChainId, RawTx, SignedTx, TxId, TxPayload, TxPreview, UnsignedTx,
    };

    #[test]
    fn an_unsigned_transaction_takes_its_chain_from_the_preview_it_showed() {
        let unsigned = UnsignedTx {
            preview: TxPreview {
                chain: ChainId::Bitcoin,
                asset: AssetId::Native(ChainId::Bitcoin),
                from: vec![String::from("from")],
                to: String::from("to"),
                destination_tag: None,
                amount: Amount::from_base_units(1),
                fee: Amount::from_base_units(2),
                details: Vec::new(),
                warnings: Vec::new(),
            },
            payload: TxPayload::from_bytes(vec![9]),
        };
        assert_eq!(unsigned.chain(), ChainId::Bitcoin);
        assert_eq!(unsigned.chain(), unsigned.preview.chain);
    }

    #[test]
    fn a_signed_transaction_takes_its_chain_from_the_id_it_will_have() {
        let signed = SignedTx {
            raw: RawTx::from_bytes(vec![0xff]),
            txid: TxId::new(ChainId::Litecoin, "d4f1"),
        };
        assert_eq!(signed.chain(), ChainId::Litecoin);
        assert_eq!(signed.txid.value(), "d4f1");
    }
}
