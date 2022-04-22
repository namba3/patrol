use std::{collections::HashMap, fmt::Display};

use serde_derive::{Deserialize, Serialize};

use crate::infrastructure::toml_file_proxy::{Error as TomlProxyError, TomlFileProxy};

use crate::domain::{
    config_repository::ConfigRepository, selector::SelectorParseError, url::UrlParseError, Config,
    Id, Mode, Selector, Url,
};

#[derive(Deserialize, Serialize, Clone)]
struct TomlConfig {
    url: Url,
    selector: Selector,
    mode: Option<Mode>,
    wait_seconds: Option<u16>,
}
impl From<Config> for TomlConfig {
    fn from(c: Config) -> Self {
        let Config {
            url,
            selector,
            mode,
            wait_seconds,
        } = c;
        Self {
            url,
            selector,
            mode: mode.into(),
            wait_seconds,
        }
    }
}
impl Into<Config> for TomlConfig {
    fn into(self) -> Config {
        let Self {
            url,
            selector,
            mode,
            wait_seconds,
        } = self;
        Config {
            url,
            selector,
            mode: mode.unwrap_or_default(),
            wait_seconds,
        }
    }
}

pub struct TomlConfigRepository {
    proxy: TomlFileProxy<HashMap<Id, TomlConfig>>,
}
impl TomlConfigRepository {
    pub async fn new(path: &str) -> Result<Self, Error> {
        let mut proxy = TomlFileProxy::new(path).await?;
        proxy.load().await?;

        Ok(Self { proxy })
    }

    /// Updates the inner hashmap and returns the old element.
    fn update_map(&mut self, id: Id, config: Config) -> RestoreInfo {
        let old_data = self
            .proxy
            .get_cache_mut()
            .unwrap()
            .insert(id.clone(), config.into());
        RestoreInfo { id, data: old_data }
    }

    fn delete_map(&mut self, id: Id) -> RestoreInfo {
        let old_data = self.proxy.get_cache_mut().unwrap().remove(&id);
        RestoreInfo { id, data: old_data }
    }

    fn restore(&mut self, restore_info: RestoreInfo) {
        let RestoreInfo { id, data } = restore_info;
        match data {
            Some(data) => {
                let _ = self.proxy.get_cache_mut().unwrap().insert(id, data);
            }
            None => {
                let _ = self.proxy.get_cache_mut().unwrap().remove(&id);
            }
        }
    }
}

#[async_trait::async_trait]
impl ConfigRepository for TomlConfigRepository {
    type Error = Error;

    async fn get_all(&mut self) -> Result<HashMap<Id, Config>, Self::Error> {
        let map = self.proxy.get_cache().unwrap();
        let map = map
            .into_iter()
            .map(|(id, config)| (id.clone(), config.clone().into()))
            .collect();
        Ok(map)
    }

    async fn update(&mut self, id: Id, config: Config) -> Result<(), Self::Error> {
        let restore_info = self.update_map(id, config);

        if let Err(e) = self.proxy.save().await {
            self.restore(restore_info);
            Err(e.into())
        } else {
            Ok(())
        }
    }

    async fn delete(&mut self, id: Id) -> Result<Option<Config>, Self::Error> {
        let restore_info = self.delete_map(id);

        if let Err(e) = self.proxy.save().await {
            self.restore(restore_info);
            Err(e.into())
        } else {
            Ok(restore_info.data.map(|x| x.into()))
        }
    }
}

struct RestoreInfo {
    id: Id,
    data: Option<TomlConfig>,
}

#[derive(Debug)]
pub enum Error {
    TomlProxyError(TomlProxyError),
    UrlParseError(UrlParseError),
    SelectorParseError(SelectorParseError),
}
impl Display for Error {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}
impl std::error::Error for Error {}
impl From<TomlProxyError> for Error {
    fn from(e: TomlProxyError) -> Self {
        Error::TomlProxyError(e)
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
