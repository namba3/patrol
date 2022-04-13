use std::{collections::HashMap, fmt::Display, io::SeekFrom};

use log::{debug, info, warn};
use serde_derive::Deserialize;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

use crate::domain::{data_repository::DataRepository, hash::FromHashStrError, Data, Hash};

#[derive(Deserialize)]
struct TomlData {
    hash: Option<String>,
    last_updated: Option<String>,
    last_checked: Option<String>,
}

pub struct TomlDataRepository {
    file: File,
    map: HashMap<String, Data>,
}
impl TomlDataRepository {
    pub async fn new(path: &str) -> Result<Self, Error> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .await?;

        let mut toml = String::new();
        file.read_to_string(&mut toml).await?;

        let toml_map: HashMap<String, TomlData> = toml::from_str(&toml)?;

        let mut map = HashMap::new();
        for (key, toml_data) in toml_map.into_iter() {
            let TomlData {
                hash,
                last_updated,
                last_checked,
            } = toml_data;

            let hash = hash.map(|x| Hash::from_hash_str(&x)).transpose()?;

            let _ = map.insert(
                key,
                Data {
                    hash,
                    last_checked,
                    last_updated,
                },
            );
        }
        Ok(Self { file, map })
    }
}

#[async_trait::async_trait]
impl DataRepository for TomlDataRepository {
    type Error = std::io::Error;

    async fn get_all(&mut self) -> Result<HashMap<String, Data>, Self::Error> {
        Ok(self.map.clone())
    }

    async fn update(&mut self, key: String, content: String) -> Result<(), Self::Error> {
        let Self { file, map } = self;

        let now = chrono::Utc::now().format("%Y-%m-%dT%T%Z").to_string();

        let data = map.entry(key.clone()).or_insert_with(|| Default::default());
        data.last_checked = now.into();

        let key = &key;

        if content.trim_start().trim_end().len() <= 0 {
            warn!("[{key}]: ignore empty content.");
        } else {
            let hash = Hash::new(content.as_bytes());

            if data.hash.as_ref() != Some(&hash) {
                data.last_updated = data.last_checked.clone();
                info!("[{key}]: updated.");
            } else {
                info!("[{key}]: not yet updated.");
            }
            data.hash = hash.into();

            debug!("[{key}]:\n{}", content);
        }

        let toml = toml::to_string_pretty(map).unwrap();
        file.seek(SeekFrom::Start(0)).await?;
        file.set_len(0).await?;
        file.write_all(toml.as_bytes()).await?;

        file.flush().await
    }
}

#[derive(Debug)]
pub enum Error {
    IoError(std::io::Error),
    TomlError(toml::de::Error),
    HashError(FromHashStrError),
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
impl From<FromHashStrError> for Error {
    fn from(e: FromHashStrError) -> Self {
        Error::HashError(e)
    }
}
