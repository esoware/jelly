use std::fmt;

/// An Electrum protocol version as `major.minor[.patch]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProtocolVersion {
    major: u16,
    minor: u16,
    patch: u16,
}

impl ProtocolVersion {
    pub const V1_4: Self = Self::new(1, 4);
    pub const V1_6: Self = Self::new(1, 6);
    /// 1.4 is the oldest version still widely deployed.
    pub const MINIMUM: Self = Self::V1_4;
    pub const MAXIMUM: Self = Self::V1_6;

    const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor, patch: 0 }
    }

    /// A version with no minor part is refused, because the protocol always
    /// spells both. The patch part is optional, and servers between 1.4 and 1.6
    /// answer with versions such as 1.4.2 and 1.5.3.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = match parts.next() {
            Some(part) => part.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self { major, minor, patch })
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.patch == 0 {
            write!(f, "{}.{}", self.major, self.minor)
        } else {
            write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protocol_version_parses_orders_and_displays() {
        assert!(ProtocolVersion::V1_4 < ProtocolVersion::V1_6);
        assert_eq!(ProtocolVersion::parse("1.6"), Some(ProtocolVersion::V1_6));
        assert_eq!(ProtocolVersion::parse(" 1.4 "), Some(ProtocolVersion::V1_4));
        assert_eq!(ProtocolVersion::parse("1"), None);
        assert_eq!(ProtocolVersion::parse("one.six"), None);
        assert_eq!(ProtocolVersion::V1_6.to_string(), "1.6");
    }

    #[test]
    fn a_patch_part_orders_after_its_minor_and_survives_display() {
        let patched = ProtocolVersion::parse("1.4.2").expect("a three-part version");
        assert!(ProtocolVersion::V1_4 < patched && patched < ProtocolVersion::V1_6);
        assert_eq!(patched.to_string(), "1.4.2");
        assert_eq!(ProtocolVersion::parse("1.6.0"), Some(ProtocolVersion::V1_6));
        assert_eq!(ProtocolVersion::parse("1.4.2.1"), None);
        assert_eq!(ProtocolVersion::parse("1.4."), None);
    }
}
