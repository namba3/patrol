pub mod hash;
pub mod id;
pub mod selector;
pub mod timestamp;
pub mod url;

pub use self::hash::Hash;
pub use self::id::Id;
pub use self::selector::Selector;
pub use self::timestamp::{Duration, Timestamp};
pub use self::url::Url;

use serde_derive::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Config {
    pub url: Url,
    pub selector: Selector,
    pub mode: Mode,
    pub wait_seconds: Option<u16>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct Data {
    pub hash: Option<Hash>,
    pub last_updated: Option<Timestamp>,
    #[serde(default)]
    pub last_checked: Option<Timestamp>,
    #[serde(default)]
    pub last_attempted: Option<Timestamp>,
    #[serde(default)]
    pub last_success: Option<Timestamp>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
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
