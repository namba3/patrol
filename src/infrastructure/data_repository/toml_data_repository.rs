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

use crate::domain::{Data, DataRepository, Hash, Id, Timestamp};

pub struct TomlDataRepository {
    file: File,
    map: HashMap<Id, Data>,
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

        let map: HashMap<Id, Data> = toml::from_str(&toml)?;

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
    fn update_map(&mut self, id: Id, content: String, now: Timestamp) -> RestoreInfo {
        let mut data = self
            .map
            .get_mut(&id)
            .map(|x| x.clone())
            .unwrap_or_else(|| Data {
                hash: None,
                last_updated: None,
                last_checked: now,
            });

        data.last_checked = now;

        let content = content.trim_start().trim_end();

        if content.len() <= 0 {
            warn!("[{id}]: ignore empty content.");
        } else {
            let hash = Hash::new(content.as_bytes());

            if data.hash.as_ref() != Some(&hash) {
                data.last_updated = now.into();
                info!(
                    "[{id}]: {}",
                    ansi_term::Color::Fixed(15).bold().paint("updated.")
                );
            } else {
                info!(
                    "[{id}]: {}",
                    ansi_term::Color::Fixed(8).paint("not yet updated.")
                );
            }
            data.hash = hash.into();

            debug!("[{id}]:\n{}", content);
        }

        let old_data = self.map.insert(id.clone(), data);
        RestoreInfo { id, data: old_data }
    }

    fn restore(&mut self, restore_info: RestoreInfo) {
        let RestoreInfo { id, data } = restore_info;
        match data {
            Some(data) => {
                let _ = self.map.insert(id, data);
            }
            None => {
                let _ = self.map.remove(&id);
            }
        }
    }
}

#[async_trait::async_trait]
impl DataRepository for TomlDataRepository {
    type Error = std::io::Error;

    async fn get(&mut self, id: Id) -> Result<Option<Data>, Self::Error> {
        let data = self.map.get(&id).map(|x| x.clone());
        Ok(data)
    }

    async fn get_multiple(&mut self, ids: HashSet<Id>) -> Result<HashMap<Id, Data>, Self::Error> {
        let iter = ids.into_iter().filter_map(|id| {
            let data = self.map.get(&id);
            data.map(|data| (id, data.clone()))
        });
        Ok(iter.collect())
    }

    async fn get_all(&mut self) -> Result<HashMap<Id, Data>, Self::Error> {
        Ok(self.map.clone())
    }

    async fn update(&mut self, id: Id, content: String) -> Result<(), Self::Error> {
        let now = Timestamp::now();
        let restore_info = self.update_map(id, content, now);

        if let Err(e) = self.save().await {
            self.restore(restore_info);
            Err(e.into())
        } else {
            Ok(())
        }
    }

    async fn update_multiple(&mut self, map: HashMap<Id, String>) -> Result<(), Self::Error> {
        let now = Timestamp::now();

        let mut restore_infos = Vec::with_capacity(map.len());
        for (id, content) in map.into_iter() {
            let restore_info = self.update_map(id, content, now);
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
    id: Id,
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
