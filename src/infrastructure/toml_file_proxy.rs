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
    pub async fn new(path: &str) -> Result<Self, Error> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .await?;

        let mut toml = String::new();
        file.read_to_string(&mut toml).await?;

        let _data: T = toml::from_str(&toml)?;

        Ok(Self { file, cache: None })
    }

    /// Load data from the file to cache, and returns the cached data
    pub async fn load(&mut self) -> Result<&T, Error> {
        let mut toml = String::new();
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

        let toml = toml::to_string_pretty(cache).unwrap();

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
    CacheEmpty,
}
impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::IoError(e) => f.write_fmt(format_args!("IO error: {e}")),
            Error::TomlError(e) => f.write_fmt(format_args!("Toml error: {e}")),
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
impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::TomlError(e)
    }
}
