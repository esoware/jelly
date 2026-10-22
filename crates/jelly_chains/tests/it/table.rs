use jelly_chains::chains;
use jelly_core::{AddressError, ChainId, KeyError, KeyKind, test_util::key_material};

/// The Ethereum key the all-abandon BIP-39 test phrase derives at `m/44'/60'/0'/0/0`,
/// and its address.
const HEX_KEY: &str = "1ab42cc412b618bdea3a599e3c9bae199ebf030895b039e9db1e30dafb12b727";
const ETHEREUM_ADDRESS: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";

/// BIP-84 publishes this WIF for `m/84'/0'/0'/0/0` of the all-abandon phrase.
const BIP84_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";
/// The same phrase's keys at `m/44'/145'/0'/0/0`, `m/44'/2'/0'/0/0`, and
/// `m/44'/3'/0'/0/0`.
const BITCOIN_CASH_WIF: &str = "KxbEv3FeYig2afQp7QEA9R3gwqdTBFwAJJ6Ma7j1SkmZoxC9bAXZ";
const LITECOIN_WIF: &str = "T5b4RiWRs7XG8xZ2bCHBoJcn4JrpMTbGRFYXgoZHd7nD8izwqhMK";
const DOGECOIN_WIF: &str = "QPkeC1ZfHx3c9g7WTj9cQ8gnvk2iSAfAcbq1aVAWjNTwDAKfZUzx";

/// A published Monero seed, from knaccc/subaddress-js.
const MONERO_SEED: &str = "vary ambush western rafts session laboratory jerseys napkin muffin exult tolerant efficient ensign fewest sulking wonders pledge alerts acquire tubes rogue outbreak lifestyle lopped pledge";

/// The PBKDF2 test vector of the Web3 Secret Storage Definition.
const KEYSTORE: &str = r#"{"crypto":{"cipher":"aes-128-ctr","cipherparams":{"iv":"6087dab2f9fdbbfaddc31a909735c1e6"},"ciphertext":"5318b4d5bcd28de64ee5559e671353e16f075ecae9f99c7a79a38af5f869aa46","kdf":"pbkdf2","kdfparams":{"c":262144,"dklen":32,"prf":"hmac-sha256","salt":"ae3cd4e7013836a3df6bd7241b12db061dbe2c6785853cce422d148a624ce0bd"},"mac":"517ead924a9d0dc3124507e3393d175ce3ff7c1e96529c6c555ce9e51205e9b2"},"id":"3198bc9c-6672-5ab3-d995-4942343ae5b6","version":3}"#;

/// The address [`HEX_KEY`] has on `chain`, in the chain's default script flavour.
fn hex_address(chain: ChainId) -> Result<String, KeyError> {
    let imported = chains().import_key(chain, HEX_KEY, None)?;
    let script = chains().spec(chain).resolve_script(None, imported.named_script)?;
    chains().entry(chain).address(&imported.material, script)
}

/// An address on `chain` in its default script flavour, made without an import, which a
/// chain that holds no standalone keys refuses.
fn default_address(chain: ChainId) -> Result<String, KeyError> {
    let spec = chains().spec(chain);
    let script = spec.resolve_script(None, None)?;
    chains().entry(chain).address(&key_material(spec.key_kind, 7), script)
}

#[test]
fn every_entry_sits_at_its_own_chains_position() {
    for (entry, chain) in chains().entries().iter().zip(ChainId::ALL) {
        assert_eq!(entry.spec().chain, chain);
        assert_eq!(chains().entry(chain).spec().chain, chain);
        assert_eq!(chains().spec(chain).chain, chain);
    }
}

/// Ethereum and BNB Smart Chain share the `ethereum` scheme, so neither refuses the
/// other's.
#[test]
fn a_payment_uri_of_another_chain_is_the_wrong_network() {
    let addresses: Vec<(ChainId, String)> =
        ChainId::ALL.into_iter().map(|chain| (chain, default_address(chain).unwrap())).collect();
    for chain in ChainId::ALL {
        let own_scheme = chains().spec(chain).uri_scheme;
        for (other, address) in &addresses {
            let scheme = chains().spec(*other).uri_scheme;
            if scheme == own_scheme {
                continue;
            }
            let target = address.strip_prefix(&format!("{scheme}:")).unwrap_or(address);
            let uri = format!("{scheme}:{target}");
            let refused = chains().validate_address(chain, &uri);
            let Err(KeyError::Address(AddressError::WrongNetwork { chain: refused_on })) = refused
            else {
                panic!("{uri} on {chain:?}: {refused:?}");
            };
            assert_eq!(refused_on, chain, "{uri}");
        }
    }
}

#[test]
fn a_key_of_another_chain_names_the_format_it_is_in() {
    let solana_keypair =
        chains().entry(ChainId::Solana).export_key(&key_material(KeyKind::Ed25519Seed, 7)).unwrap();
    let cases = [
        (ChainId::Bitcoin, LITECOIN_WIF.to_owned(), "Litecoin WIF key"),
        (ChainId::Bitcoin, DOGECOIN_WIF.to_owned(), "Dogecoin WIF key"),
        (ChainId::Ethereum, solana_keypair.as_str().to_owned(), "Solana keypair"),
        (ChainId::Solana, KEYSTORE.to_owned(), "keystore file"),
        (ChainId::Ethereum, BIP84_WIF.to_owned(), "Bitcoin WIF key"),
        (ChainId::Litecoin, BIP84_WIF.to_owned(), "Bitcoin WIF key"),
        (ChainId::Litecoin, format!("p2wpkh:{BIP84_WIF}"), "Bitcoin WIF key"),
        (ChainId::Dogecoin, BITCOIN_CASH_WIF.to_owned(), "Bitcoin WIF key"),
    ];
    for (chain, input, format) in cases {
        let refused = chains().import_key(chain, &input, None);
        let Err(KeyError::WrongChain { format: named, chain: refused_on }) = refused else {
            panic!("{format} on {chain:?}: {refused:?}");
        };
        assert_eq!((named, refused_on), (format, chain));
    }
}

/// Monero holds no standalone keys, so naming its seed would send the user to a chain
/// that cannot take it.
#[test]
fn a_monero_seed_is_named_as_no_chains_key() {
    for chain in [ChainId::Bitcoin, ChainId::Solana, ChainId::Monero] {
        let refused = chains().import_key(chain, MONERO_SEED, None);
        assert!(matches!(refused, Err(KeyError::UnknownFormat)), "{chain:?}: {refused:?}");
    }
}

/// Bitcoin Cash and Zcash write their keys with Bitcoin's WIF version byte.
#[test]
fn a_bitcoin_version_wif_imports_on_every_chain_that_shares_it() {
    for wif in [BIP84_WIF, BITCOIN_CASH_WIF] {
        let secrets: Vec<Vec<u8>> = [ChainId::Bitcoin, ChainId::BitcoinCash, ChainId::Zcash]
            .into_iter()
            .map(|chain| {
                chains().import_key(chain, wif, None).unwrap().material.with_bytes(<[u8]>::to_vec)
            })
            .collect();
        assert!(secrets.iter().all(|secret| *secret == secrets[0]), "{wif}");
    }
}

#[test]
fn bare_hex_imports_to_a_valid_address_on_every_chain_that_holds_standalone_keys() {
    let can_hold_keys = |chain: &ChainId| chains().spec(*chain).can_hold_standalone_keys();
    for chain in ChainId::ALL.into_iter().filter(can_hold_keys) {
        let address = hex_address(chain).unwrap();
        let validated = chains().validate_address(chain, &address).unwrap();
        assert_eq!(validated.value(), address, "{chain:?}");
    }
    assert_eq!(hex_address(ChainId::Ethereum).unwrap(), ETHEREUM_ADDRESS);
}

/// TRON shares Ethereum's curve and key, so its addresses encode the same public key.
#[test]
fn ethereum_hex_imports_on_bsc_and_tron_as_the_same_key() {
    for input in [HEX_KEY.to_owned(), format!("0x{HEX_KEY}")] {
        for chain in [ChainId::Ethereum, ChainId::Bsc, ChainId::Tron] {
            let imported = chains().import_key(chain, &input, None).unwrap();
            assert_eq!(
                chains().entry(ChainId::Ethereum).address(&imported.material, None).unwrap(),
                ETHEREUM_ADDRESS,
                "{chain:?}"
            );
        }
    }
}

/// TON's adapter reads the comment out of the request, so a send keeps the request. Every
/// other chain's send carries the canonical address alone.
#[test]
fn a_send_keeps_the_payment_request_only_where_the_adapter_reads_it() {
    const TON_ADDRESS: &str = "UQDv2YSmlrlLH3hLNOVxC8FcQf4F9eGNs4vb2zKma4txo6i3";
    let request = format!(" ton://transfer/{TON_ADDRESS}?text=order%207 ");
    assert_eq!(chains().payment_target(ChainId::Ton, &request).unwrap(), request.trim());
    assert_eq!(chains().payment_target(ChainId::Ton, TON_ADDRESS).unwrap(), TON_ADDRESS);

    let lowercase = ETHEREUM_ADDRESS.to_ascii_lowercase();
    let uri = format!("ethereum:{lowercase}?value=1");
    assert_eq!(chains().payment_target(ChainId::Ethereum, &uri).unwrap(), ETHEREUM_ADDRESS);
    assert!(matches!(
        chains().payment_target(ChainId::Ethereum, "bitcoin:bc1q"),
        Err(KeyError::Address(AddressError::WrongNetwork { chain: ChainId::Ethereum }))
    ));
}
