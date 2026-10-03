use std::collections::{HashMap, HashSet};

use log::{debug, info};

use crate::infrastructure::toml_file_proxy::{Error, TomlFileProxy};

use crate::domain::{Data, DataRepository, Hash, Id, PollResultBatch, Timestamp};

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

    async fn save_with_rollback(
        &mut self,
        restore_infos: Vec<RestoreInfo>,
    ) -> Result<Vec<RestoreInfo>, Error> {
        if let Err(error) = self.proxy.save().await {
            for restore_info in restore_infos {
                self.restore(restore_info);
            }
            Err(error)
        } else {
            Ok(restore_infos)
        }
    }

    // Updates the cached entry and returns its prior value and hash-change status.
    fn update_map(&mut self, id: Id, hash: Hash, now: Timestamp) -> (RestoreInfo, bool) {
        let cache = self.proxy.get_cache_mut().unwrap();
        let entry = cache.entry(id.clone());
        let mut data = match &entry {
            std::collections::hash_map::Entry::Occupied(entry) => entry.get().clone(),
            std::collections::hash_map::Entry::Vacant(_) => Data::default(),
        };
        let changed = data.hash.as_ref() != Some(&hash);

        data.last_checked = Some(now);
        data.last_attempted = Some(now);
        data.last_success = Some(now);
        data.consecutive_failures = 0;
        data.last_error = None;

        if changed {
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

        let old_data = match entry {
            std::collections::hash_map::Entry::Occupied(mut entry) => Some(entry.insert(data)),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let _ = entry.insert(data);
                None
            }
        };
        (RestoreInfo { id, data: old_data }, changed)
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
        let data = map.get(&id).cloned();
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
            .iter()
            .map(|(id, data)| (id.clone(), data.clone()))
            .collect();
        Ok(map)
    }

    async fn update(&mut self, id: Id, hash: Hash) -> Result<Option<Timestamp>, Self::Error> {
        let mut batch = self
            .record_poll_results(
                HashMap::from([(id.clone(), hash)]),
                HashSet::new(),
                HashMap::new(),
            )
            .await?;
        Ok(batch
            .changed_at
            .remove(&id)
            .expect("batch result includes the updated ID"))
    }

    async fn record_success(&mut self, id: Id) -> Result<(), Self::Error> {
        self.record_successes(HashSet::from([id])).await
    }

    async fn record_successes(&mut self, ids: HashSet<Id>) -> Result<(), Self::Error> {
        self.record_poll_results(HashMap::new(), ids, HashMap::new())
            .await
            .map(|_| ())
    }

    async fn record_failure(&mut self, id: Id, error: String) -> Result<u32, Self::Error> {
        let mut batch = self
            .record_poll_results(
                HashMap::new(),
                HashSet::new(),
                HashMap::from([(id.clone(), error)]),
            )
            .await?;
        Ok(batch
            .failure_counts
            .remove(&id)
            .expect("batch result includes the failed ID"))
    }

    async fn record_failures(
        &mut self,
        failures: HashMap<Id, String>,
    ) -> Result<HashMap<Id, u32>, Self::Error> {
        self.record_poll_results(HashMap::new(), HashSet::new(), failures)
            .await
            .map(|result| result.failure_counts)
    }

    async fn record_poll_results(
        &mut self,
        hashes: HashMap<Id, Hash>,
        empty_successes: HashSet<Id>,
        failures: HashMap<Id, String>,
    ) -> Result<PollResultBatch, Self::Error> {
        if hashes.is_empty() && empty_successes.is_empty() && failures.is_empty() {
            return Ok(PollResultBatch::default());
        }

        let now = Timestamp::now();
        let mut changed_at = HashMap::with_capacity(hashes.len());
        let mut failure_counts = HashMap::with_capacity(failures.len());
        let outcome_count = hashes.len() + empty_successes.len() + failures.len();
        let mut seen = HashSet::with_capacity(outcome_count);
        let mut restore_infos = Vec::with_capacity(outcome_count);

        for (id, hash) in hashes {
            let (restore_info, changed) = self.update_map(id, hash, now);
            let _ = changed_at.insert(restore_info.id.clone(), changed.then_some(now));
            if seen.insert(restore_info.id.clone()) {
                restore_infos.push(restore_info);
            }
        }

        for id in empty_successes {
            let cache = self.proxy.get_cache_mut().unwrap();
            let entry = cache.entry(id.clone());
            let restore_info = if seen.insert(id.clone()) {
                let data = match &entry {
                    std::collections::hash_map::Entry::Occupied(entry) => Some(entry.get().clone()),
                    std::collections::hash_map::Entry::Vacant(_) => None,
                };
                Some(RestoreInfo {
                    id: id.clone(),
                    data,
                })
            } else {
                None
            };
            let data = entry.or_default();
            data.last_attempted = Some(now);
            data.last_success = Some(now);
            data.consecutive_failures = 0;
            data.last_error = None;
            if let Some(restore_info) = restore_info {
                restore_infos.push(restore_info);
            }
        }

        for (id, error) in failures {
            let cache = self.proxy.get_cache_mut().unwrap();
            let entry = cache.entry(id.clone());
            let restore_info = if seen.insert(id.clone()) {
                let data = match &entry {
                    std::collections::hash_map::Entry::Occupied(entry) => Some(entry.get().clone()),
                    std::collections::hash_map::Entry::Vacant(_) => None,
                };
                Some(RestoreInfo {
                    id: id.clone(),
                    data,
                })
            } else {
                None
            };
            let data = entry.or_default();
            data.last_attempted = Some(now);
            data.consecutive_failures = data.consecutive_failures.saturating_add(1);
            data.last_error = Some(error);
            let _ = failure_counts.insert(id, data.consecutive_failures);
            if let Some(restore_info) = restore_info {
                restore_infos.push(restore_info);
            }
        }

        self.save_with_rollback(restore_infos).await?;
        Ok(PollResultBatch {
            changed_at,
            failure_counts,
        })
    }

    async fn update_multiple(&mut self, map: HashMap<Id, Hash>) -> Result<(), Self::Error> {
        self.update_multiple_with_timestamps(map).await.map(|_| ())
    }

    async fn update_multiple_with_timestamps(
        &mut self,
        map: HashMap<Id, Hash>,
    ) -> Result<HashMap<Id, Option<Timestamp>>, Self::Error> {
        self.record_poll_results(map, HashSet::new(), HashMap::new())
            .await
            .map(|result| result.changed_at)
    }

    async fn delete(&mut self, id: Id) -> Result<Option<Data>, Self::Error> {
        let restore_info = self.delete_map(id);
        let restore_info = self
            .save_with_rollback(vec![restore_info])
            .await?
            .pop()
            .unwrap();
        Ok(restore_info.data)
    }
}

struct RestoreInfo {
    id: Id,
    data: Option<Data>,
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf};

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

    #[tokio::test]
    async fn update_multiple_persists_items_and_reports_changed_timestamps() {
        let path = temp_data_path();
        let path_string = path.to_str().unwrap();
        let first_id = Id::try_from("first-page".to_owned()).unwrap();
        let second_id = Id::try_from("second-page".to_owned()).unwrap();
        let updates = std::collections::HashMap::from([
            (first_id.clone(), Hash::new("first version")),
            (second_id.clone(), Hash::new("second version")),
        ]);
        let mut repository = TomlDataRepository::new(path_string).await.unwrap();

        let first_update = repository
            .update_multiple_with_timestamps(updates.clone())
            .await
            .unwrap();
        assert_eq!(first_update.len(), 2);
        assert!(first_update[&first_id].is_some());
        assert!(first_update[&second_id].is_some());

        let unchanged_update = repository
            .update_multiple_with_timestamps(updates)
            .await
            .unwrap();
        assert_eq!(unchanged_update[&first_id], None);
        assert_eq!(unchanged_update[&second_id], None);

        drop(repository);
        let mut reloaded = TomlDataRepository::new(path_string).await.unwrap();
        assert!(reloaded.get(first_id).await.unwrap().is_some());
        assert!(reloaded.get(second_id).await.unwrap().is_some());

        drop(reloaded);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn record_failures_persists_multiple_items_and_returns_counts() {
        let path = temp_data_path();
        let path_string = path.to_str().unwrap();
        let first_id = Id::try_from("first-page".to_owned()).unwrap();
        let second_id = Id::try_from("second-page".to_owned()).unwrap();
        let failures = std::collections::HashMap::from([
            (first_id.clone(), "first error".to_owned()),
            (second_id.clone(), "second error".to_owned()),
        ]);
        let mut repository = TomlDataRepository::new(path_string).await.unwrap();

        let counts = repository.record_failures(failures).await.unwrap();
        assert_eq!(counts[&first_id], 1);
        assert_eq!(counts[&second_id], 1);

        drop(repository);
        let mut reloaded = TomlDataRepository::new(path_string).await.unwrap();
        assert_eq!(
            reloaded
                .get(first_id)
                .await
                .unwrap()
                .unwrap()
                .last_error
                .as_deref(),
            Some("first error")
        );
        assert_eq!(
            reloaded
                .get(second_id)
                .await
                .unwrap()
                .unwrap()
                .last_error
                .as_deref(),
            Some("second error")
        );

        drop(reloaded);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn record_successes_persists_multiple_empty_poll_results() {
        let path = temp_data_path();
        let path_string = path.to_str().unwrap();
        let first_id = Id::try_from("first-page".to_owned()).unwrap();
        let second_id = Id::try_from("second-page".to_owned()).unwrap();
        let mut repository = TomlDataRepository::new(path_string).await.unwrap();

        repository
            .record_failures(std::collections::HashMap::from([
                (first_id.clone(), "first error".to_owned()),
                (second_id.clone(), "second error".to_owned()),
            ]))
            .await
            .unwrap();
        repository
            .record_successes(HashSet::from([first_id.clone(), second_id.clone()]))
            .await
            .unwrap();

        drop(repository);
        let mut reloaded = TomlDataRepository::new(path_string).await.unwrap();
        for id in [first_id, second_id] {
            let data = reloaded.get(id).await.unwrap().unwrap();
            assert_eq!(data.last_success, data.last_attempted);
            assert_eq!(data.consecutive_failures, 0);
            assert_eq!(data.last_error, None);
            assert_eq!(data.hash, None);
            assert_eq!(data.last_checked, None);
        }

        drop(reloaded);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn record_poll_results_persists_mixed_outcomes_together() {
        let path = temp_data_path();
        let path_string = path.to_str().unwrap();
        let changed_id = Id::try_from("changed-page".to_owned()).unwrap();
        let empty_id = Id::try_from("empty-page".to_owned()).unwrap();
        let failed_id = Id::try_from("failed-page".to_owned()).unwrap();
        let mut repository = TomlDataRepository::new(path_string).await.unwrap();

        let batch = repository
            .record_poll_results(
                std::collections::HashMap::from([(changed_id.clone(), Hash::new("new content"))]),
                HashSet::from([empty_id.clone()]),
                std::collections::HashMap::from([(failed_id.clone(), "request failed".to_owned())]),
            )
            .await
            .unwrap();
        assert!(batch.changed_at[&changed_id].is_some());
        assert_eq!(batch.failure_counts[&failed_id], 1);

        drop(repository);
        let mut reloaded = TomlDataRepository::new(path_string).await.unwrap();
        let changed = reloaded.get(changed_id).await.unwrap().unwrap();
        assert_eq!(changed.hash, Some(Hash::new("new content")));
        assert!(changed.last_checked.is_some());

        let empty = reloaded.get(empty_id).await.unwrap().unwrap();
        assert_eq!(empty.hash, None);
        assert_eq!(empty.last_checked, None);
        assert!(empty.last_success.is_some());

        let failed = reloaded.get(failed_id).await.unwrap().unwrap();
        assert_eq!(failed.consecutive_failures, 1);
        assert_eq!(failed.last_error.as_deref(), Some("request failed"));

        drop(reloaded);
        std::fs::remove_file(path).unwrap();
    }
}
