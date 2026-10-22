use std::collections::BTreeSet;

use bitcoin::bip32::{self, ChildNumber};
use jelly_core::{DerivationPath, KeyError};

/// The placeholder a chain spec's default path puts where the address index goes.
const INDEX_PLACEHOLDER: &str = "{index}";

/// BIP-32 stores depth in one byte, so a path holds at most this many
/// components.
const MAX_PATH_DEPTH: usize = 255;

/// The one way text becomes a path. [`bip32::DerivationPath`]'s own `FromStr` refuses
/// `M` and `H`, accepts a `+` before an index, and reports no position, so it is never
/// used.
///
/// The leading `m` or `M` is optional, and `'`, `h`, or `H` marks a hardened component,
/// as does the typographic apostrophe U+2019 that note apps put in place of `'`.
/// Whitespace around the path is trimmed, but a space inside it is a malformed
/// component. Components are numbered from zero after the `m`, so an error names the
/// same component whether or not the `m` was written.
pub fn parse_path(text: &str) -> Result<bip32::DerivationPath, KeyError> {
    let components = parts(text)
        .enumerate()
        .map(|(component, part)| parse_component(part, component))
        .collect::<Result<Vec<_>, _>>()?;
    if components.len() > MAX_PATH_DEPTH {
        return Err(KeyError::PathTooDeep { got: components.len(), max: MAX_PATH_DEPTH });
    }
    Ok(components.into())
}

/// The trimmed text split at `/`, without its optional leading `m` or `M`.
fn parts(text: &str) -> impl Iterator<Item = &str> {
    let mut parts = text.trim().split('/').peekable();
    parts.next_if(|part| matches!(*part, "m" | "M"));
    parts
}

fn parse_component(part: &str, component: usize) -> Result<ChildNumber, KeyError> {
    let malformed = || KeyError::InvalidPath { component };
    let (digits, is_hardened) = match part.strip_suffix(['\'', '\u{2019}', 'h', 'H']) {
        Some(digits) => (digits, true),
        None => (part, false),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(malformed());
    }
    let index: u32 = digits.parse().map_err(|_| malformed())?;
    if is_hardened {
        ChildNumber::from_hardened_idx(index)
    } else {
        ChildNumber::from_normal_idx(index)
    }
    .map_err(|_| malformed())
}

/// The `m/44'/0'` form that address rows store and the interface shows, so it must not
/// change.
pub fn stored_path(path: &bip32::DerivationPath) -> DerivationPath {
    if path.is_master() {
        DerivationPath::new("m")
    } else {
        DerivationPath::new(format!("m/{path}"))
    }
}

/// The BIP-44 purpose a path names, which is its first component when that is
/// hardened.
pub fn purpose_of(path: &bip32::DerivationPath) -> Option<u32> {
    match path.as_ref().first() {
        Some(ChildNumber::Hardened { index }) => Some(*index),
        Some(ChildNumber::Normal { .. }) | None => None,
    }
}

/// The index without its hardened marker.
pub const fn child_index(child: ChildNumber) -> u32 {
    match child {
        ChildNumber::Normal { index } | ChildNumber::Hardened { index } => index,
    }
}

/// Everything up to and including the path's last hardened component, the deepest node
/// whose xpub is safe to export.
pub fn account_prefix(path: &bip32::DerivationPath) -> Option<bip32::DerivationPath> {
    let components = path.as_ref();
    let last = components.iter().rposition(ChildNumber::is_hardened)?;
    let keep = last.checked_add(1)?;
    components.get(..keep).map(bip32::DerivationPath::from)
}

/// A path family with one varying component, `prefix / index / suffix`.
///
/// [`PathTemplate::index_of`] recognizes an instance and [`PathTemplate::next_index`]
/// finds the first unused index among stored paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTemplate {
    prefix: Vec<ChildNumber>,
    is_index_hardened: bool,
    suffix: Vec<ChildNumber>,
}

impl PathTemplate {
    /// Reads a chain spec's default path, such as `m/44'/501'/{index}'/0'`, where the
    /// placeholder may carry a hardened marker.
    ///
    /// A template with no placeholder, or more than one, is a build defect in the spec
    /// that wrote it, so it is reported as internal.
    pub fn parse(template: &str) -> Result<Self, KeyError> {
        let mut prefix = Vec::new();
        let mut suffix = Vec::new();
        let mut varying = None;
        for (component, part) in parts(template).enumerate() {
            if let Some(marker) = part.strip_prefix(INDEX_PLACEHOLDER) {
                if varying.is_some() {
                    return Err(KeyError::internal("a path template names two indices"));
                }
                varying = Some(match marker {
                    "" => false,
                    "'" | "h" | "H" => true,
                    _ => return Err(KeyError::InvalidPath { component }),
                });
                continue;
            }
            let child = parse_component(part, component)?;
            if varying.is_some() { suffix.push(child) } else { prefix.push(child) }
        }
        let is_index_hardened =
            varying.ok_or(KeyError::internal("a path template names no index"))?;
        Ok(Self { prefix, is_index_hardened, suffix })
    }

    /// `m/purpose'/coin'/0'/0/i`, the receive chain of the first account.
    pub fn bip44(purpose: u32, coin: u32) -> Self {
        Self {
            prefix: vec![
                ChildNumber::Hardened { index: purpose },
                ChildNumber::Hardened { index: coin },
                ChildNumber::Hardened { index: 0 },
                ChildNumber::Normal { index: 0 },
            ],
            is_index_hardened: false,
            suffix: Vec::new(),
        }
    }

    pub fn path_at(&self, index: u32) -> Result<bip32::DerivationPath, KeyError> {
        let varying = if self.is_index_hardened {
            ChildNumber::from_hardened_idx(index)
        } else {
            ChildNumber::from_normal_idx(index)
        }
        .map_err(|_| KeyError::ChildIndexOutOfRange { index })?;
        Ok(self
            .prefix
            .iter()
            .copied()
            .chain([varying])
            .chain(self.suffix.iter().copied())
            .collect())
    }

    /// The varying index of `path`, when `path` is an instance of this family.
    pub fn index_of(&self, path: &bip32::DerivationPath) -> Option<u32> {
        let (head, rest) = path.as_ref().split_at_checked(self.prefix.len())?;
        if head != self.prefix.as_slice() {
            return None;
        }
        let (varying, tail) = rest.split_first()?;
        if varying.is_hardened() != self.is_index_hardened || tail != self.suffix.as_slice() {
            return None;
        }
        Some(child_index(*varying))
    }

    /// The lowest index this family does not already have a path for. Paths
    /// that are not instances of the family, including unparsable ones, are
    /// ignored.
    pub fn next_index<'a>(&self, used_paths: impl IntoIterator<Item = &'a DerivationPath>) -> u32 {
        let taken_indices: BTreeSet<u32> = used_paths
            .into_iter()
            .filter_map(|path| parse_path(path.as_str()).ok())
            .filter_map(|path| self.index_of(&path))
            .collect();
        (0..=u32::MAX).find(|index| !taken_indices.contains(index)).unwrap_or(u32::MAX)
    }
}
