use std::{collections::HashMap, fmt::Display};

use serde_derive::Deserialize;
use tokio::{
    fs::{File, OpenOptions},
    io::AsyncReadExt,
};

use crate::domain::{
    config_repository::ConfigRepository, selector::SelectorParseError, url::UrlParseError, Config,
    Mode, Selector, Url,
};

#[derive(Deserialize)]
struct TomlConfig {
    url: Url,
    selector: Selector,
    mode: Option<Mode>,
    wait_seconds: Option<u16>,
}

pub struct TomlConfigRepository {
    file: File,
    map: HashMap<String, Config>,
}
impl TomlConfigRepository {
    pub async fn new(path: &str) -> Result<Self, Error> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .await?;

        let mut toml = String::new();
        file.read_to_string(&mut toml).await?;

        let toml_map: HashMap<String, TomlConfig> = toml::from_str(&toml)?;

        let mut map = HashMap::new();
        for (key, toml_config) in toml_map.into_iter() {
            let TomlConfig {
                url,
                selector,
                mode,
                wait_seconds,
            } = toml_config;
            let mode = mode.unwrap_or_default();
            let wait_seconds = wait_seconds;
            let _ = map.insert(
                key,
                Config {
                    url,
                    selector,
                    mode,
                    wait_seconds,
                },
            );
        }

        Ok(Self { file, map })
    }
}

#[async_trait::async_trait]
impl ConfigRepository for TomlConfigRepository {
    type Error = std::io::Error;

    async fn get_all(&mut self) -> Result<HashMap<String, Config>, Self::Error> {
        Ok(self.map.clone())
    }
}

#[derive(Debug)]
pub enum Error {
    IoError(std::io::Error),
    TomlError(toml::de::Error),
    UrlParseError(UrlParseError),
    SelectorParseError(SelectorParseError),
}
impl Display for Error {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IoError(e)
    }
}
impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::TomlError(e)
    }
}
impl From<UrlParseError> for Error {
    fn from(e: UrlParseError) -> Self {
        Error::UrlParseError(e)
    }
}
impl From<SelectorParseError> for Error {
    fn from(e: SelectorParseError) -> Self {
        Error::SelectorParseError(e)
    }
}
