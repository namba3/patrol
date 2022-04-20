use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    io::SeekFrom,
};

use log::{debug, info, warn};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

use crate::domain::{Data, DataRepository, Hash, Timestamp};

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

        let map: HashMap<String, Data> = toml::from_str(&toml)?;

        Ok(Self { file, map })
    }

    async fn save(&mut self) -> Result<(), std::io::Error> {
        let Self { file, map } = self;
        let toml = toml::to_string_pretty(map).unwrap();

        file.seek(SeekFrom::Start(0)).await?;
        file.set_len(0).await?;
        file.write_all(toml.as_bytes()).await?;

        file.flush().await
    }

    // Updates the inner hashmap and returns the old element.
    fn update_map(&mut self, key: String, content: String, now: Timestamp) -> RestoreInfo {
        let mut data = self
            .map
            .get_mut(&key)
            .map(|x| x.clone())
            .unwrap_or_else(|| Data {
                hash: None,
                last_updated: None,
                last_checked: now,
            });

        data.last_checked = now;

        let content = content.trim_start().trim_end();

        if content.len() <= 0 {
            warn!("[{key}]: ignore empty content.");
        } else {
            let hash = Hash::new(content.as_bytes());

            if data.hash.as_ref() != Some(&hash) {
                data.last_updated = now.into();
                info!(
                    "[{key}]: {}",
                    ansi_term::Color::Fixed(15).bold().paint("updated.")
                );
            } else {
                info!(
                    "[{key}]: {}",
                    ansi_term::Color::Fixed(8).paint("not yet updated.")
                );
            }
            data.hash = hash.into();

            debug!("[{key}]:\n{}", content);
        }

        let old_data = self.map.insert(key.clone(), data);
        RestoreInfo {
            key,
            data: old_data,
        }
    }

    fn restore(&mut self, restore_info: RestoreInfo) {
        let RestoreInfo { key, data } = restore_info;
        match data {
            Some(data) => {
                let _ = self.map.insert(key, data);
            }
            None => {
                let _ = self.map.remove(&key);
            }
        }
    }
}

#[async_trait::async_trait]
impl DataRepository for TomlDataRepository {
    type Error = std::io::Error;

    async fn get(&mut self, key: String) -> Result<Option<Data>, Self::Error> {
        let data = self.map.get(&key).map(|x| x.clone());
        Ok(data)
    }

    async fn get_multiple(
        &mut self,
        keys: HashSet<String>,
    ) -> Result<HashMap<String, Data>, Self::Error> {
        let iter = keys.into_iter().filter_map(|key| {
            let data = self.map.get(&key);
            data.map(|data| (key, data.clone()))
        });
        Ok(iter.collect())
    }

    async fn get_all(&mut self) -> Result<HashMap<String, Data>, Self::Error> {
        Ok(self.map.clone())
    }

    async fn update(&mut self, key: String, content: String) -> Result<(), Self::Error> {
        let now = Timestamp::now();
        let restore_info = self.update_map(key, content, now);

        if let Err(e) = self.save().await {
            self.restore(restore_info);
            Err(e.into())
        } else {
            Ok(())
        }
    }

    async fn update_multiple(&mut self, map: HashMap<String, String>) -> Result<(), Self::Error> {
        let now = Timestamp::now();

        let mut restore_infos = Vec::with_capacity(map.len());
        for (key, content) in map.into_iter() {
            let restore_info = self.update_map(key, content, now);
            restore_infos.push(restore_info);
        }

        if let Err(e) = self.save().await {
            for restore_info in restore_infos.into_iter() {
                self.restore(restore_info);
            }
            Err(e.into())
        } else {
            Ok(())
        }
    }
}

struct RestoreInfo {
    key: String,
    data: Option<Data>,
}

#[derive(Debug)]
pub enum Error {
    IoError(std::io::Error),
    TomlError(toml::de::Error),
}
impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::IoError(e) => f.write_fmt(format_args!("IO error: {e}")),
            Error::TomlError(e) => f.write_fmt(format_args!("Toml error: {e}")),
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
