use serde::Deserialize;
use serde_derive::Serialize;

pub mod hash;
pub mod selector;
pub mod url;

pub use self::hash::Hash;
pub use self::selector::Selector;
pub use self::url::Url;

#[derive(Serialize, Debug, Clone)]
pub struct Config {
    pub url: Url,
    pub selector: Selector,
    pub mode: Mode,
    pub wait_seconds: Option<u16>,
}

#[derive(Serialize, Default, Debug, Clone)]
pub struct Data {
    pub hash: Option<Hash>,
    pub last_updated: Option<String>,
    pub last_checked: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Simple,
    Full,
}
impl Default for Mode {
    fn default() -> Self {
        Mode::Full
    }
}
