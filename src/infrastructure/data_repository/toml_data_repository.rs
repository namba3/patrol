use std::collections::{HashMap, HashSet};

use log::{debug, info};

use crate::infrastructure::toml_file_proxy::{Error, TomlFileProxy};

use crate::domain::{Data, DataRepository, Hash, Id, Timestamp};

pub struct TomlDataRepository {
    proxy: TomlFileProxy<HashMap<Id, Data>>,
}
impl TomlDataRepository {
    pub async fn new(path: &str) -> Result<Self, Error> {
        let mut proxy = TomlFileProxy::<HashMap<Id, Data>>::new(path).await?;
        let map = proxy.load().await?;
        debug!("{} has {} data entries.", path, map.len());

        Ok(Self { proxy })
    }

    // Updates the inner hashmap and returns the old element.
    fn update_map(&mut self, id: Id, hash: Hash, now: Timestamp) -> RestoreInfo {
        let mut data = self
            .proxy
            .get_cache_mut()
            .unwrap()
            .get_mut(&id)
            .map(|x| x.clone())
            .unwrap_or_else(|| Data {
                hash: None,
                last_updated: None,
                last_checked: now,
            });

        data.last_checked = now;

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

        let old_data = self.proxy.get_cache_mut().unwrap().insert(id.clone(), data);
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
impl DataRepository for TomlDataRepository {
    type Error = Error;

    async fn get(&mut self, id: Id) -> Result<Option<Data>, Self::Error> {
        let map = self.proxy.get_cache().unwrap();
        let data = map.get(&id).map(|x| x.clone());
        Ok(data)
    }

    async fn get_multiple(&mut self, ids: HashSet<Id>) -> Result<HashMap<Id, Data>, Self::Error> {
        let map = self.proxy.get_cache().unwrap();
        let iter = ids.into_iter().filter_map(|id| {
            let data = map.get(&id);
            data.map(|data| (id, data.clone()))
        });
        Ok(iter.collect())
    }

    async fn get_all(&mut self) -> Result<HashMap<Id, Data>, Self::Error> {
        let map = self.proxy.get_cache().unwrap();
        let map = map
            .into_iter()
            .map(|(id, data)| (id.clone(), data.clone()))
            .collect();
        Ok(map)
    }

    async fn update(&mut self, id: Id, hash: Hash) -> Result<(), Self::Error> {
        let now = Timestamp::now();
        let restore_info = self.update_map(id, hash, now);

        if let Err(e) = self.proxy.save().await {
            self.restore(restore_info);
            Err(e.into())
        } else {
            Ok(())
        }
    }

    async fn update_multiple(&mut self, map: HashMap<Id, Hash>) -> Result<(), Self::Error> {
        let now = Timestamp::now();

        let mut restore_infos = Vec::with_capacity(map.len());
        for (id, hash) in map.into_iter() {
            let restore_info = self.update_map(id, hash, now);
            restore_infos.push(restore_info);
        }

        if let Err(e) = self.proxy.save().await {
            for restore_info in restore_infos.into_iter() {
                self.restore(restore_info);
            }
            Err(e.into())
        } else {
            Ok(())
        }
    }

    async fn delete(&mut self, id: Id) -> Result<Option<Data>, Self::Error> {
        let restore_info = self.delete_map(id);

        if let Err(e) = self.proxy.save().await {
            self.restore(restore_info);
            Err(e.into())
        } else {
            Ok(restore_info.data)
        }
    }
}

struct RestoreInfo {
    id: Id,
    data: Option<Data>,
}
