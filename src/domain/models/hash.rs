use std::{fmt::Display};

use serde::{Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

impl Serialize for Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<[u8; 32]> for Hash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
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
