use sha2::{Digest, Sha256};
use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Calculate hash value from the given bytes.
    ///
    /// If you construct `Hash` from the string that represents the hash value, please use `from_hash_str`
    pub fn new<Bytes: AsRef<[u8]>>(v: Bytes) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(v.as_ref());
        Hash(hasher.finalize().into())
    }

    /// Try to construct `Hash` from the string that represents the hash value.
    pub fn from_hash_str(s: &str) -> Result<Self, FromHashStrError> {
        fn f(b: u8) -> Option<u8> {
            if b.is_ascii_digit() {
                Some(b - 0x30)
            } else if b.is_ascii_hexdigit() {
                Some((b % 0x10) + 0x09)
            } else {
                None
            }
        }

        if s.len() != 64 {
            return Err(FromHashStrError {});
        }

        let mut buf = [0u8; 32];
        for (i, x) in s.as_bytes().chunks_exact(2).enumerate() {
            let a = f(x[0]).ok_or(FromHashStrError {})?;
            let b = f(x[1]).ok_or(FromHashStrError {})?;
            buf[i] = a * 0x10 + b;
        }

        Ok(Hash(buf))
    }
}

impl Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for x in self.0.iter() {
            f.write_fmt(format_args!("{x:02x}"))?;
        }

        Ok(())
    }
}

impl From<[u8; 32]> for Hash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl serde::Serialize for Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> serde::Deserialize<'de> for Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(HashVisitor)
    }
}

struct HashVisitor;
impl<'de> serde::de::Visitor<'de> for HashVisitor {
    type Value = Hash;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "hex digits of length 64")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        match Hash::from_hash_str(s) {
            Ok(x) => Ok(x),
            Err(_e) => Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(s),
                &self,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FromHashStrError {}
impl Display for FromHashStrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hash string must be hex digits of length 64")
    }
}

impl std::error::Error for FromHashStrError {}

#[cfg(test)]
mod tests {
    use super::Hash;

    #[test]
    fn hashes_bytes_as_sha256() {
        let hash = Hash::new("abc");

        assert_eq!(
            hash.to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parses_uppercase_hex_and_formats_lowercase() {
        let hash =
            Hash::from_hash_str("BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD")
                .unwrap();

        assert_eq!(
            hash.to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rejects_invalid_hash_strings() {
        assert!(Hash::from_hash_str("abc").is_err());
        assert!(Hash::from_hash_str(&"g".repeat(64)).is_err());
    }

    #[test]
    fn serde_round_trip_uses_hex_string() {
        let hash = Hash::new("patrol");
        let encoded = serde_json::to_string(&hash).unwrap();
        let decoded: Hash = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, hash);
        assert_eq!(encoded, format!("\"{hash}\""));
    }
}
