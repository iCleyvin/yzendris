//! SHA-256 certificate fingerprints — the identity of a paired machine.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// SHA-256 digest of a peer's DER-encoded certificate.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of_der(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }

    /// Short human-checkable form, e.g. `3f9a-c21e-07bd`.
    pub fn short(&self) -> String {
        let b = &self.0;
        format!(
            "{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseFingerprintError;

impl fmt::Display for ParseFingerprintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected `sha256:` followed by 64 hex digits")
    }
}

impl std::error::Error for ParseFingerprintError {}

impl FromStr for Fingerprint {
    type Err = ParseFingerprintError;

    /// Accepts `sha256:<64 hex>`, with or without `:` between byte pairs
    /// (the colon-separated form is what Yzendris 0.1 printed).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let hex = s
            .strip_prefix("sha256:")
            .or_else(|| s.strip_prefix("SHA256:"))
            .unwrap_or(s);
        let mut out = [0u8; 32];
        let mut digits = hex.bytes().filter(|b| *b != b':');
        for byte in &mut out {
            let hi = digits
                .next()
                .and_then(hex_val)
                .ok_or(ParseFingerprintError)?;
            let lo = digits
                .next()
                .and_then(hex_val)
                .ok_or(ParseFingerprintError)?;
            *byte = hi << 4 | lo;
        }
        if digits.next().is_some() {
            return Err(ParseFingerprintError);
        }
        Ok(Self(out))
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl Serialize for Fingerprint {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_text() {
        let fp = Fingerprint::of_der(b"certificate bytes");
        let text = fp.to_string();
        assert_eq!(text.len(), 7 + 64);
        assert_eq!(text.parse::<Fingerprint>().unwrap(), fp);
    }

    #[test]
    fn accepts_legacy_colon_form() {
        let fp = Fingerprint([0xab; 32]);
        let legacy = format!("sha256:{}", vec!["ab"; 32].join(":"));
        assert_eq!(legacy.parse::<Fingerprint>().unwrap(), fp);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!("sha256:abcd".parse::<Fingerprint>().is_err());
        assert!(format!("sha256:{}", "zz".repeat(32))
            .parse::<Fingerprint>()
            .is_err());
        assert!(format!("sha256:{}", "ab".repeat(33))
            .parse::<Fingerprint>()
            .is_err());
    }
}
