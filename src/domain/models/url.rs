use serde::Deserialize;
use serde_derive::Serialize;

use std::fmt::Display;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Url(String);
impl Url {
    pub fn new(url: String) -> Result<Self, UrlParseError> {
        url::Url::parse(url.as_ref())
            .map(|_| Self(url))
            .map_err(|_| UrlParseError)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Into<String> for Url {
    fn into(self) -> String {
        self.0
    }
}
impl AsRef<str> for Url {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl<'de> Deserialize<'de> for Url {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(UrlVisitor)
    }
}

struct UrlVisitor;
impl<'de> serde::de::Visitor<'de> for UrlVisitor {
    type Value = Url;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "valid URL")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        match Url::new(s.to_owned()) {
            Ok(x) => Ok(x),
            Err(_e) => Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(s),
                &self,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UrlParseError;
impl Display for UrlParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed to parse the URL.")
    }
}
