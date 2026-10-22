//! Fixtures the unit tests share.

use bitcoin::bip32;

use crate::path::parse_path;

pub(crate) fn path(text: &str) -> bip32::DerivationPath {
    parse_path(text).expect("the fixture path parses")
}
