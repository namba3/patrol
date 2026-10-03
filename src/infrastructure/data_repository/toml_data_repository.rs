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
                last_checked: None,
                last_attempted: None,
                last_success: None,
                consecutive_failures: 0,
                last_error: None,
            });

        data.last_checked = Some(now);
        data.last_attempted = Some(now);
        data.last_success = Some(now);
        data.consecutive_failures = 0;
        data.last_error = None;

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

    async fn update(&mut self, id: Id, hash: Hash) -> Result<Option<Timestamp>, Self::Error> {
        let now = Timestamp::now();
        let restore_info = self.update_map(id.clone(), hash.clone(), now);

        if let Err(e) = self.proxy.save().await {
            self.restore(restore_info);
            Err(e.into())
        } else {
            let prev_hash = restore_info.data.map(|x| x.hash).flatten();
            let timestamp = if Some(&hash) != prev_hash.as_ref() {
                Some(now)
            } else {
                None
            };
            Ok(timestamp)
        }
    }

    async fn record_success(&mut self, id: Id) -> Result<(), Self::Error> {
        let now = Timestamp::now();
        let previous_data = self.proxy.get_cache().unwrap().get(&id).cloned();
        let data = self
            .proxy
            .get_cache_mut()
            .unwrap()
            .entry(id.clone())
            .or_insert_with(|| Data {
                hash: None,
                last_updated: None,
                last_checked: None,
                last_attempted: None,
                last_success: None,
                consecutive_failures: 0,
                last_error: None,
            });

        data.last_attempted = Some(now);
        data.last_success = Some(now);
        data.consecutive_failures = 0;
        data.last_error = None;

        if let Err(error) = self.proxy.save().await {
            match previous_data {
                Some(data) => {
                    let _ = self.proxy.get_cache_mut().unwrap().insert(id, data);
                }
                None => {
                    let _ = self.proxy.get_cache_mut().unwrap().remove(&id);
                }
            }
            return Err(error.into());
        }

        Ok(())
    }

    async fn record_failure(&mut self, id: Id, error: String) -> Result<u32, Self::Error> {
        let now = Timestamp::now();
        let previous_data = self.proxy.get_cache().unwrap().get(&id).cloned();
        let data = self
            .proxy
            .get_cache_mut()
            .unwrap()
            .entry(id.clone())
            .or_insert_with(|| Data {
                hash: None,
                last_updated: None,
                last_checked: None,
                last_attempted: None,
                last_success: None,
                consecutive_failures: 0,
                last_error: None,
            });

        data.last_attempted = Some(now);
        data.consecutive_failures = data.consecutive_failures.saturating_add(1);
        data.last_error = Some(error);
        let consecutive_failures = data.consecutive_failures;

        if let Err(error) = self.proxy.save().await {
            match previous_data {
                Some(data) => {
                    let _ = self.proxy.get_cache_mut().unwrap().insert(id, data);
                }
                None => {
                    let _ = self.proxy.get_cache_mut().unwrap().remove(&id);
                }
            }
            return Err(error.into());
        }

        Ok(consecutive_failures)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::TomlDataRepository;
    use crate::domain::{DataRepository, Hash, Id};

    fn temp_data_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "patrol-data-repository-{}.toml",
            uuid::Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn loads_data_files_without_poll_status_fields() {
        let path = temp_data_path();
        let path_string = path.to_str().unwrap();
        let legacy_hash = "00".repeat(32);
        std::fs::write(
            &path,
            format!(
                "[LegacyPage]\nhash = \"{legacy_hash}\"\nlast_updated = \"2026-10-02T12:00:00Z\"\nlast_checked = \"2026-10-02T12:00:00Z\"\n"
            ),
        )
        .unwrap();

        let mut repository = TomlDataRepository::new(path_string).await.unwrap();
        let data = repository
            .get(Id::try_from("LegacyPage".to_owned()).unwrap())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(data.hash, Some(Hash::from_hash_str(&legacy_hash).unwrap()));
        assert_eq!(data.last_attempted, None);
        assert_eq!(data.last_success, None);
        assert_eq!(data.consecutive_failures, 0);
        assert_eq!(data.last_error, None);

        drop(repository);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn update_reports_changes_and_persists_data() {
        let path = temp_data_path();
        let path_string = path.to_str().unwrap();
        let id = Id::try_from("example-page".to_owned()).unwrap();

        let mut repository = TomlDataRepository::new(path_string).await.unwrap();
        let first_hash = Hash::new("first version");
        repository
            .update(id.clone(), first_hash.clone())
            .await
            .unwrap()
            .expect("first observation should be reported as an update");

        let unchanged = repository.update(id.clone(), first_hash).await.unwrap();
        assert_eq!(unchanged, None);

        let second_hash = Hash::new("second version");
        let second_updated = repository
            .update(id.clone(), second_hash.clone())
            .await
            .unwrap()
            .expect("a changed hash should be reported as an update");

        drop(repository);

        let mut reloaded = TomlDataRepository::new(path_string).await.unwrap();
        let data = reloaded.get(id.clone()).await.unwrap().unwrap();
        assert_eq!(data.hash, Some(second_hash));
        assert_eq!(data.last_updated, Some(second_updated));
        assert!(data.last_checked.unwrap() >= second_updated);
        assert_eq!(data.last_success, Some(second_updated));
        assert_eq!(data.consecutive_failures, 0);

        let failure_count = reloaded
            .record_failure(id.clone(), "temporary error".to_owned())
            .await
            .unwrap();
        assert_eq!(failure_count, 1);
        let failed_data = reloaded.get(id.clone()).await.unwrap().unwrap();
        assert_eq!(failed_data.last_error.as_deref(), Some("temporary error"));
        assert_eq!(failed_data.consecutive_failures, 1);

        let second_failure_count = reloaded
            .record_failure(id.clone(), "still failing".to_owned())
            .await
            .unwrap();
        assert_eq!(second_failure_count, 2);
        let recovered_at = reloaded
            .update(id.clone(), Hash::new("second version"))
            .await
            .unwrap();
        assert_eq!(recovered_at, None);
        let recovered_data = reloaded.get(id.clone()).await.unwrap().unwrap();
        assert_eq!(recovered_data.consecutive_failures, 0);
        assert_eq!(recovered_data.last_error, None);

        let deleted = reloaded.delete(id.clone()).await.unwrap().unwrap();
        assert_eq!(deleted.hash, Some(Hash::new("second version")));
        assert!(reloaded.delete(id.clone()).await.unwrap().is_none());

        drop(reloaded);
        let mut after_delete = TomlDataRepository::new(path_string).await.unwrap();
        assert!(after_delete.get(id).await.unwrap().is_none());
        drop(after_delete);
        std::fs::remove_file(path).unwrap();
    }
}
