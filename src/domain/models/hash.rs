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
