use serde::Deserialize;
use serde_derive::Serialize;

use std::fmt::Display;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

impl<'de> Deserialize<'de> for Selector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(SelectorVisitor)
    }
}

struct SelectorVisitor;
impl<'de> serde::de::Visitor<'de> for SelectorVisitor {
    type Value = Selector;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "valid css selector")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        match Selector::new(s.to_owned()) {
            Ok(x) => Ok(x),
            Err(_e) => Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(s),
                &self,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectorParseError;
impl Display for SelectorParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed to parse the selector.")
    }
}

#[cfg(test)]
mod tests {
    use super::Selector;

    #[test]
    fn accepts_valid_css_selector() {
        let selector = Selector::new("main article h1.title".to_owned()).unwrap();

        assert_eq!(selector.as_str(), "main article h1.title");
    }

    #[test]
    fn rejects_invalid_css_selector() {
        assert!(Selector::new("div[".to_owned()).is_err());
    }

    #[test]
    fn serde_rejects_invalid_css_selector() {
        assert!(serde_json::from_str::<Selector>("\"div[\"").is_err());
    }
}
