use serde_derive::Serialize;
use std::fmt::Display;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Selector(String);
impl Selector {
    pub fn new(selector: String) -> Result<Self, SelectorParseError> {
        if let Err(_) = scraper::Selector::parse(selector.as_str()) {
            return Err(SelectorParseError);
        }

        Ok(Self(selector))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Into<String> for Selector {
    fn into(self) -> String {
        self.0
    }
}
impl AsRef<str> for Selector {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectorParseError;
impl Display for SelectorParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed to parse the selector.")
    }
}
