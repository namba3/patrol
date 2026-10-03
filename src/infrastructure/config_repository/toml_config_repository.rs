use std::{collections::HashMap, fmt::Display};

use log::debug;
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
        let mut proxy = TomlFileProxy::<HashMap<Id, TomlConfig>>::new(path).await?;
        let map = proxy.load().await?;
        debug!("{} has {} configurations.", path, map.len());

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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TomlProxyError(error) => write!(f, "TOML file error: {error}"),
            Error::UrlParseError(error) => write!(f, "URL parse error: {error}"),
            Error::SelectorParseError(error) => write!(f, "selector parse error: {error}"),
        }
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::{ConfigRepository, Id, Mode};

    use super::{Error, TomlConfigRepository};

    fn temp_config_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "patrol-config-repository-{}.toml",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn repository_errors_have_readable_display_messages() {
        assert_eq!(
            Error::TomlProxyError(crate::infrastructure::toml_file_proxy::Error::CacheEmpty)
                .to_string(),
            "TOML file error: Cache is empty."
        );
        assert_eq!(
            Error::UrlParseError(crate::domain::url::UrlParseError).to_string(),
            "URL parse error: failed to parse the URL."
        );
        assert_eq!(
            Error::SelectorParseError(crate::domain::selector::SelectorParseError).to_string(),
            "selector parse error: failed to parse the selector."
        );
    }

    #[tokio::test]
    async fn loads_default_and_explicit_modes() {
        let path = temp_config_path();
        let source = r#"
[DefaultMode]
url = "https://example.com/default"
selector = "main"

[SimpleMode]
url = "https://example.com/simple"
selector = ".status"
mode = "simple"
wait_seconds = 3
"#;
        std::fs::write(&path, source).unwrap();

        let mut repository = TomlConfigRepository::new(path.to_str().unwrap())
            .await
            .unwrap();
        let configs = repository.get_all().await.unwrap();
        let default = &configs[&Id::try_from("DefaultMode".to_owned()).unwrap()];
        let simple = &configs[&Id::try_from("SimpleMode".to_owned()).unwrap()];

        assert_eq!(default.mode, Mode::Full);
        assert_eq!(default.wait_seconds, None);
        assert_eq!(simple.mode, Mode::Simple);
        assert_eq!(simple.wait_seconds, Some(3));

        drop(repository);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_url() {
        let path = temp_config_path();
        std::fs::write(
            &path,
            "[Broken]\nurl = \"not a url\"\nselector = \"main\"\n",
        )
        .unwrap();

        let result = TomlConfigRepository::new(path.to_str().unwrap()).await;

        assert!(result.is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_selector() {
        let path = temp_config_path();
        std::fs::write(
            &path,
            "[Broken]\nurl = \"https://example.com\"\nselector = \"div[\"\n",
        )
        .unwrap();

        let result = TomlConfigRepository::new(path.to_str().unwrap()).await;

        assert!(result.is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn update_and_delete_persist_config_changes() {
        let path = temp_config_path();
        std::fs::write(
            &path,
            "[Page]\nurl = \"https://example.com/old\"\nselector = \"main\"\nmode = \"full\"\n",
        )
        .unwrap();

        let path_string = path.to_str().unwrap();
        let id = Id::try_from("Page".to_owned()).unwrap();
        let mut repository = TomlConfigRepository::new(path_string).await.unwrap();
        let replacement = crate::domain::Config {
            url: crate::domain::Url::new("https://example.com/new".to_owned()).unwrap(),
            selector: crate::domain::Selector::new(".content".to_owned()).unwrap(),
            mode: Mode::Simple,
            wait_seconds: Some(5),
        };

        repository
            .update(id.clone(), replacement.clone())
            .await
            .unwrap();
        let updated = repository.get_all().await.unwrap();
        assert_eq!(updated[&id].url.as_str(), "https://example.com/new");
        assert_eq!(updated[&id].selector.as_str(), ".content");
        assert_eq!(updated[&id].mode, Mode::Simple);
        assert_eq!(updated[&id].wait_seconds, Some(5));

        let deleted = repository.delete(id.clone()).await.unwrap().unwrap();
        assert_eq!(deleted.url, replacement.url);
        assert!(repository.delete(id).await.unwrap().is_none());
        drop(repository);

        let mut reloaded = TomlConfigRepository::new(path_string).await.unwrap();
        assert!(reloaded.get_all().await.unwrap().is_empty());

        drop(reloaded);
        std::fs::remove_file(path).unwrap();
    }
}
