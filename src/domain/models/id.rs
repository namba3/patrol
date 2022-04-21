use serde::Deserialize;
use serde_derive::Serialize;
use std::fmt::Display;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(String);
impl Id {
    pub fn new() -> Self {
        let mut buf = [0u8; 32];
        let s = uuid::Uuid::new_v4().simple().encode_lower(&mut buf);
        Self(s.to_owned())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl AsRef<str> for Id {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl TryFrom<String> for Id {
    type Error = FromStringError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if 0 < value.len() {
            Ok(Self(value))
        } else {
            Err(FromStringError {})
        }
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(IdVisitor)
    }
}

struct IdVisitor;
impl<'de> serde::de::Visitor<'de> for IdVisitor {
    type Value = Id;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a string of length more than 1")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        match Id::try_from(s.to_owned()) {
            Ok(x) => Ok(x),
            Err(_e) => Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(s),
                &self,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FromStringError {}
impl Display for FromStringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Id must be a string that the length is more than 0.")
    }
}

impl std::error::Error for FromStringError {}
