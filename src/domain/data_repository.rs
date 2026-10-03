use crate::domain::{Data, Hash, Id, Timestamp};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub struct PollResultBatch {
    pub changed_at: HashMap<Id, Option<Timestamp>>,
    pub failure_counts: HashMap<Id, u32>,
}

#[async_trait::async_trait]
pub trait DataRepository {
    type Error: std::error::Error + Send;

    async fn get(&mut self, id: Id) -> Result<Option<Data>, Self::Error>;
    async fn get_multiple(&mut self, ids: HashSet<Id>) -> Result<HashMap<Id, Data>, Self::Error>;
    async fn get_all(&mut self) -> Result<HashMap<Id, Data>, Self::Error>;

    async fn update(&mut self, id: Id, hash: Hash) -> Result<Option<Timestamp>, Self::Error>;
    async fn record_success(&mut self, id: Id) -> Result<(), Self::Error>;
    async fn record_failure(&mut self, id: Id, error: String) -> Result<u32, Self::Error>;
    async fn update_multiple(&mut self, map: HashMap<Id, Hash>) -> Result<(), Self::Error>;

    /// Updates multiple hashes and reports the timestamp only for changed values.
    async fn update_multiple_with_timestamps(
        &mut self,
        map: HashMap<Id, Hash>,
    ) -> Result<HashMap<Id, Option<Timestamp>>, Self::Error> {
        if map.is_empty() {
            return Ok(HashMap::new());
        }

        let ids = map.keys().cloned().collect::<HashSet<_>>();
        let previous = self.get_multiple(ids.clone()).await?;
        self.update_multiple(map.clone()).await?;
        let current = self.get_multiple(ids).await?;

        let mut updated_at = HashMap::with_capacity(map.len());
        for (id, hash) in map {
            let previous_hash = previous.get(&id).and_then(|data| data.hash.as_ref());
            let timestamp = if previous_hash == Some(&hash) {
                None
            } else {
                current.get(&id).and_then(|data| data.last_updated)
            };
            let _ = updated_at.insert(id, timestamp);
        }
        Ok(updated_at)
    }

    /// Records several failed polls and returns each updated consecutive-failure count.
    async fn record_failures(
        &mut self,
        failures: HashMap<Id, String>,
    ) -> Result<HashMap<Id, u32>, Self::Error> {
        let mut counts = HashMap::with_capacity(failures.len());
        for (id, error) in failures {
            let count = self.record_failure(id.clone(), error).await?;
            let _ = counts.insert(id, count);
        }
        Ok(counts)
    }

    /// Records several successful polls whose extracted content was empty.
    async fn record_successes(&mut self, ids: HashSet<Id>) -> Result<(), Self::Error> {
        for id in ids {
            self.record_success(id).await?;
        }
        Ok(())
    }

    /// Persists all outcomes from one polling cycle.
    ///
    /// Each ID should appear in at most one of the three outcome collections.
    async fn record_poll_results(
        &mut self,
        hashes: HashMap<Id, Hash>,
        empty_successes: HashSet<Id>,
        failures: HashMap<Id, String>,
    ) -> Result<PollResultBatch, Self::Error> {
        let changed_at = self.update_multiple_with_timestamps(hashes).await?;
        self.record_successes(empty_successes).await?;
        let failure_counts = self.record_failures(failures).await?;
        Ok(PollResultBatch {
            changed_at,
            failure_counts,
        })
    }

    async fn delete(&mut self, id: Id) -> Result<Option<Data>, Self::Error>;
}
