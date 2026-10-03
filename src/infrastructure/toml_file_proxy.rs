use std::{fmt::Display, io::SeekFrom};

use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

pub struct TomlFileProxy<T> {
    file: File,
    cache: Option<T>,
}

impl<T> TomlFileProxy<T>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    /// Create a new proxy to the toml file.
    pub async fn new(path: &str) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .await?;

        Ok(Self { file, cache: None })
    }

    /// Load data from the file to cache, and returns the cached data
    pub async fn load(&mut self) -> Result<&T, Error> {
        let mut toml = String::new();

        self.file.seek(SeekFrom::Start(0)).await?;
        self.file.read_to_string(&mut toml).await?;

        self.cache = toml::from_str::<T>(&toml)?.into();

        Ok(self.cache.as_ref().unwrap())
    }

    /// Save the cached data to the file
    pub async fn save(&mut self) -> Result<(), Error> {
        let Self { file, cache } = self;
        let cache = match cache {
            Some(c) => c,
            None => return Err(Error::CacheEmpty),
        };

        let toml = toml::to_string_pretty(cache)?;

        file.seek(SeekFrom::Start(0)).await?;
        file.set_len(0).await?;
        file.write_all(toml.as_bytes()).await?;

        file.flush().await?;

        Ok(())
    }

    pub fn get_cache(&self) -> Option<&T> {
        self.cache.as_ref()
    }

    pub fn get_cache_mut(&mut self) -> Option<&mut T> {
        self.cache.as_mut()
    }

    pub fn update_cache(&mut self, data: T) {
        self.cache = data.into();
    }

    pub async fn get_cache_or_load(&mut self) -> Result<&T, Error> {
        if self.cache.is_some() {
            return Ok(self.cache.as_ref().unwrap());
        }
        self.load().await
    }

    pub async fn save_with_data(&mut self, data: T) -> Result<(), Error> {
        self.cache = data.into();
        self.save().await
    }
}

#[derive(Debug)]
pub enum Error {
    IoError(std::io::Error),
    TomlError(toml::de::Error),
    TomlSerializeError(toml::ser::Error),
    CacheEmpty,
}
impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::IoError(e) => f.write_fmt(format_args!("IO error: {e}")),
            Error::TomlError(e) => f.write_fmt(format_args!("Toml error: {e}")),
            Error::TomlSerializeError(e) => {
                f.write_fmt(format_args!("Toml serialization error: {e}"))
            }
            Error::CacheEmpty => f.write_fmt(format_args!("Cache is empty.")),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IoError(e)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::{Error, TomlFileProxy};

    struct FailsToSerialize;

    impl serde::Serialize for FailsToSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional serialization failure",
            ))
        }
    }

    impl<'de> serde::Deserialize<'de> for FailsToSerialize {
        fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Ok(Self)
        }
    }

    fn temp_toml_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "patrol-toml-file-proxy-{}.toml",
            uuid::Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn save_without_cache_returns_cache_empty() {
        let path = temp_toml_path();
        let mut proxy = TomlFileProxy::<HashMap<String, String>>::new(path.to_str().unwrap())
            .await
            .unwrap();

        assert!(matches!(proxy.save().await, Err(Error::CacheEmpty)));

        drop(proxy);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn serialization_failure_is_returned_as_an_error() {
        let path = temp_toml_path();
        let mut proxy = TomlFileProxy::<FailsToSerialize>::new(path.to_str().unwrap())
            .await
            .unwrap();
        proxy.update_cache(FailsToSerialize);

        let result = proxy.save().await;

        assert!(matches!(result, Err(Error::TomlSerializeError(_))));
        drop(proxy);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn saves_cache_and_loads_it_again() {
        let path = temp_toml_path();
        let path_string = path.to_str().unwrap();
        let mut proxy = TomlFileProxy::<HashMap<String, String>>::new(path_string)
            .await
            .unwrap();
        let mut expected = HashMap::from([
            ("page".to_owned(), "a long value to be replaced".to_owned()),
            ("other".to_owned(), "kept".to_owned()),
        ]);

        proxy.save_with_data(expected.clone()).await.unwrap();
        proxy
            .get_cache_mut()
            .unwrap()
            .insert("page".to_owned(), "short".to_owned());
        expected.insert("page".to_owned(), "short".to_owned());
        proxy.save().await.unwrap();
        drop(proxy);

        let mut reloaded = TomlFileProxy::<HashMap<String, String>>::new(path_string)
            .await
            .unwrap();
        assert_eq!(reloaded.load().await.unwrap(), &expected);

        drop(reloaded);
        std::fs::remove_file(path).unwrap();
    }
}
impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::TomlError(e)
    }
}
impl From<toml::ser::Error> for Error {
    fn from(e: toml::ser::Error) -> Self {
        Error::TomlSerializeError(e)
    }
}
