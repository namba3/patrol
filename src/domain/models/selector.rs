use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    fmt::Display,
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Debug, Clone)]
pub struct Selector {
    source: String,
    parsed: Arc<scraper::Selector>,
}
impl Selector {
    pub fn new(selector: String) -> Result<Self, SelectorParseError> {
        let parsed = scraper::Selector::parse(selector.as_str()).map_err(|_| SelectorParseError)?;

        Ok(Self {
            source: selector,
            parsed: Arc::new(parsed),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub(crate) fn parsed(&self) -> &scraper::Selector {
        &self.parsed
    }
}
impl From<Selector> for String {
    fn from(selector: Selector) -> Self {
        selector.source
    }
}
impl AsRef<str> for Selector {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Serialize for Selector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl PartialEq for Selector {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}
impl Eq for Selector {}
impl PartialOrd for Selector {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Selector {
    fn cmp(&self, other: &Self) -> Ordering {
        self.source.cmp(&other.source)
    }
}
impl Hash for Selector {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source.hash(state);
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
impl std::error::Error for SelectorParseError {}

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

    #[test]
    fn serializes_as_the_original_selector_string() {
        let selector = Selector::new("main article h1.title".to_owned()).unwrap();

        assert_eq!(
            serde_json::to_string(&selector).unwrap(),
            "\"main article h1.title\""
        );
    }

    #[test]
    fn clones_share_the_parsed_selector() {
        let selector = Selector::new("main article h1.title".to_owned()).unwrap();
        let clone = selector.clone();

        assert!(std::ptr::eq(selector.parsed(), clone.parsed()));
    }
}
