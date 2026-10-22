use std::collections::{BTreeSet, HashSet};

use jelly_chains::chains;
use jelly_core::{ChainId, Transport};
use jelly_net::Endpoint;

use crate::common::registry;

#[test]
fn every_chain_ships_ascii_defaults() {
    let registry = registry();
    for chain in ChainId::ALL {
        for entry in &registry.defaults(chain).unwrap().endpoints {
            assert!(entry.url.is_ascii(), "{chain:?} has a non-ascii url");
            assert!(
                entry.note.iter().all(|note| note.is_ascii()),
                "{} has a non-ascii note",
                entry.url
            );
        }
    }
}

#[test]
fn every_default_is_canonical_and_never_plaintext() {
    let registry = registry();
    for chain in ChainId::ALL {
        for entry in &registry.defaults(chain).unwrap().endpoints {
            let endpoint = Endpoint::parse(chain, entry.transport, &entry.url)
                .unwrap_or_else(|error| panic!("{chain:?} {}: {error}", entry.url));
            assert_eq!(endpoint.url(), entry.url, "{chain:?} ships a non-canonical url");
            assert!(!endpoint.is_plaintext(), "{chain:?} ships a plaintext endpoint");
        }
    }
}

#[test]
fn no_url_repeats_inside_one_chain() {
    let registry = registry();
    for chain in ChainId::ALL {
        let mut seen: HashSet<&str> = HashSet::new();
        for entry in &registry.defaults(chain).unwrap().endpoints {
            assert!(seen.insert(entry.url.as_str()), "{chain:?} lists {} twice", entry.url);
        }
    }
}

#[test]
fn each_chain_ships_defaults_on_exactly_its_declared_transports() {
    let registry = registry();
    for chain in ChainId::ALL {
        let spec = chains().spec(chain);
        let present: BTreeSet<Transport> = registry
            .defaults(chain)
            .unwrap()
            .endpoints
            .iter()
            .map(|entry| entry.transport)
            .collect();
        for transport in &present {
            assert!(spec.transports.contains(transport), "{chain:?} ships {transport}");
        }
        for transport in spec.transports {
            assert!(present.contains(transport), "{chain:?} is missing {transport}");
        }
    }
}
