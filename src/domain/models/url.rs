use serde_derive::{Deserialize, Serialize};
use std::fmt::Display;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UrlParseError;
impl Display for UrlParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed to parse the URL.")
    }
}
