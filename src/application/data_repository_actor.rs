use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use crate::domain::{self, Id, Timestamp};
use tokio::sync::{mpsc, oneshot};

pub struct DataRepositoryActor<DataRepository> {
    inner: DataRepository,
}
impl<DataRepository> DataRepositoryActor<DataRepository>
where
    DataRepository: domain::DataRepository + Send + 'static,
{
    pub fn new(inner: DataRepository) -> Self {
        Self { inner }
    }
    pub async fn start(mut self) -> DataRepositoryActorClient<DataRepository> {
        let (tx_message, mut rx_message) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(message) = rx_message.recv().await {
                match message {
                    Message::Get { tx, id } => {
                        let result = self.inner.get(id).await;
                        let _ = tx.send(result);
                    }
                    Message::GetMultiple { tx, ids } => {
                        let result = self.inner.get_multiple(ids).await;
                        let _ = tx.send(result);
                    }
                    Message::GetAll { tx } => {
                        let result = self.inner.get_all().await;
                        let _ = tx.send(result);
                    }
                    Message::Update { tx, id, hash } => {
                        let result = self.inner.update(id, hash).await;
                        let _ = tx.send(result);
                    }
                    Message::RecordSuccess { tx, id } => {
                        let result = self.inner.record_success(id).await;
                        let _ = tx.send(result);
                    }
                    Message::RecordFailure { tx, id, error } => {
                        let result = self.inner.record_failure(id, error).await;
                        let _ = tx.send(result);
                    }
                    Message::UpdateMultiple { tx, map } => {
                        let result = self.inner.update_multiple(map).await;
                        let _ = tx.send(result);
                    }
                    Message::Delete { tx, id } => {
                        let result = self.inner.delete(id).await;
                        let _ = tx.send(result);
                    }
                }
            }
        });

        DataRepositoryActorClient { tx_message }
    }
}

enum Message<E> {
    Get {
        tx: oneshot::Sender<Result<Option<domain::Data>, E>>,
        id: Id,
    },
    GetMultiple {
        tx: oneshot::Sender<Result<HashMap<Id, domain::Data>, E>>,
        ids: HashSet<Id>,
    },
    GetAll {
        tx: oneshot::Sender<Result<HashMap<Id, domain::Data>, E>>,
    },
    Update {
        tx: oneshot::Sender<Result<Option<Timestamp>, E>>,
        id: Id,
        hash: domain::Hash,
    },
    RecordFailure {
        tx: oneshot::Sender<Result<u32, E>>,
        id: Id,
        error: String,
    },
    RecordSuccess {
        tx: oneshot::Sender<Result<(), E>>,
        id: Id,
    },
    UpdateMultiple {
        tx: oneshot::Sender<Result<(), E>>,
        map: HashMap<Id, domain::Hash>,
    },
    Delete {
        tx: oneshot::Sender<Result<Option<domain::Data>, E>>,
        id: Id,
    },
}

pub struct DataRepositoryActorClient<DataRepository: domain::DataRepository> {
    tx_message: mpsc::UnboundedSender<Message<DataRepository::Error>>,
}
impl<DataRepository: domain::DataRepository> DataRepositoryActorClient<DataRepository> {
    pub fn clone(&self) -> Self {
        let tx_message = self.tx_message.clone();
        Self { tx_message }
    }
}

#[async_trait::async_trait]
impl<DataRepository: domain::DataRepository> domain::DataRepository
    for DataRepositoryActorClient<DataRepository>
{
    type Error = Error<DataRepository::Error>;

    async fn get(&mut self, id: Id) -> Result<Option<domain::Data>, Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::Get { tx, id }) {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }

    async fn get_multiple(
        &mut self,
        ids: HashSet<Id>,
    ) -> Result<HashMap<Id, domain::Data>, Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::GetMultiple { tx, ids }) {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }

    async fn get_all(&mut self) -> Result<HashMap<Id, domain::Data>, Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::GetAll { tx }) {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }

    async fn update(
        &mut self,
        id: Id,
        hash: domain::Hash,
    ) -> Result<Option<Timestamp>, Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::Update { tx, id, hash }) {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }

    async fn record_failure(&mut self, id: Id, error: String) -> Result<u32, Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self
            .tx_message
            .send(Message::RecordFailure { tx, id, error })
        {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }

    async fn record_success(&mut self, id: Id) -> Result<(), Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::RecordSuccess { tx, id }) {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }

    async fn update_multiple(&mut self, map: HashMap<Id, domain::Hash>) -> Result<(), Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::UpdateMultiple { tx, map }) {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }

    async fn delete(&mut self, id: Id) -> Result<Option<domain::Data>, Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::Delete { tx, id }) {
            return Err(Error::ActorMessageError(ActorMessageError::SendError));
        }

        match rx.await {
            Ok(result) => result.map_err(Error::DataRepositoryError),
            Err(_e) => Err(Error::ActorMessageError(ActorMessageError::RecvError)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error<E: std::error::Error> {
    ActorMessageError(ActorMessageError),
    DataRepositoryError(E),
}
impl<E: std::error::Error> Display for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ActorMessageError(e) => f.write_fmt(format_args!("Actor message error: {e}")),
            Error::DataRepositoryError(e) => f.write_fmt(format_args!("DataRepository error: {e}")),
        }
    }
}
impl<E: std::error::Error> std::error::Error for Error<E> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorMessageError {
    SendError,
    RecvError,
}
impl Display for ActorMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActorMessageError::SendError => {
                f.write_fmt(format_args!("failed to send the message to the actor."))
            }
            ActorMessageError::RecvError => f.write_fmt(format_args!(
                "failed to receive the message from the actor."
            )),
        }
    }
}
impl std::error::Error for ActorMessageError {}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::DataRepositoryActor;
    use crate::domain::{Data, DataRepository, Hash, Id, Timestamp};

    #[derive(Default)]
    struct InMemoryRepository {
        data: HashMap<Id, Data>,
    }

    #[async_trait::async_trait]
    impl DataRepository for InMemoryRepository {
        type Error = std::io::Error;

        async fn get(&mut self, id: Id) -> Result<Option<Data>, Self::Error> {
            Ok(self.data.get(&id).cloned())
        }

        async fn get_multiple(
            &mut self,
            ids: HashSet<Id>,
        ) -> Result<HashMap<Id, Data>, Self::Error> {
            Ok(ids
                .into_iter()
                .filter_map(|id| self.data.get(&id).cloned().map(|data| (id, data)))
                .collect())
        }

        async fn get_all(&mut self) -> Result<HashMap<Id, Data>, Self::Error> {
            Ok(self.data.clone())
        }

        async fn update(&mut self, id: Id, hash: Hash) -> Result<Option<Timestamp>, Self::Error> {
            let now = Timestamp::now();
            let data = self.data.entry(id).or_default();
            let changed = data.hash.as_ref() != Some(&hash);
            if changed {
                data.last_updated = Some(now);
            }
            data.hash = Some(hash);
            data.last_checked = Some(now);
            data.last_attempted = Some(now);
            data.last_success = Some(now);
            data.consecutive_failures = 0;
            data.last_error = None;
            Ok(if changed { Some(now) } else { None })
        }

        async fn record_failure(&mut self, id: Id, error: String) -> Result<u32, Self::Error> {
            let now = Timestamp::now();
            let data = self.data.entry(id).or_default();
            data.last_attempted = Some(now);
            data.consecutive_failures = data.consecutive_failures.saturating_add(1);
            data.last_error = Some(error);
            Ok(data.consecutive_failures)
        }

        async fn record_success(&mut self, id: Id) -> Result<(), Self::Error> {
            let now = Timestamp::now();
            let data = self.data.entry(id).or_default();
            data.last_attempted = Some(now);
            data.last_success = Some(now);
            data.consecutive_failures = 0;
            data.last_error = None;
            Ok(())
        }

        async fn update_multiple(&mut self, map: HashMap<Id, Hash>) -> Result<(), Self::Error> {
            for (id, hash) in map {
                self.update(id, hash).await?;
            }
            Ok(())
        }

        async fn delete(&mut self, id: Id) -> Result<Option<Data>, Self::Error> {
            Ok(self.data.remove(&id))
        }
    }

    fn id(value: &str) -> Id {
        Id::try_from(value.to_owned()).unwrap()
    }

    #[tokio::test]
    async fn client_forwards_repository_operations_and_clone_shares_actor() {
        let mut client = DataRepositoryActor::new(InMemoryRepository::default())
            .start()
            .await;
        let first_id = id("first");
        let second_id = id("second");
        let first_hash = Hash::new("first content");
        let second_hash = Hash::new("second content");

        assert!(client.get(first_id.clone()).await.unwrap().is_none());
        let first_updated = client
            .update(first_id.clone(), first_hash.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            client
                .update(first_id.clone(), first_hash.clone())
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            client
                .record_failure(first_id.clone(), "temporary failure".to_owned())
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            client
                .update(first_id.clone(), first_hash.clone())
                .await
                .unwrap(),
            None
        );
        let first_data = client.get(first_id.clone()).await.unwrap().unwrap();
        assert_eq!(first_data.hash, Some(first_hash.clone()));
        assert_eq!(first_data.last_updated, Some(first_updated));
        assert_eq!(first_data.consecutive_failures, 0);
        assert_eq!(first_data.last_error, None);

        let updates = HashMap::from([(second_id.clone(), second_hash.clone())]);
        client.update_multiple(updates).await.unwrap();
        assert_eq!(
            client
                .record_failure(second_id.clone(), "temporary failure".to_owned())
                .await
                .unwrap(),
            1
        );
        client.record_success(second_id.clone()).await.unwrap();
        let recovered_second = client.get(second_id.clone()).await.unwrap().unwrap();
        assert_eq!(recovered_second.consecutive_failures, 0);
        assert_eq!(recovered_second.last_error, None);
        let selected = client
            .get_multiple(HashSet::from([
                first_id.clone(),
                second_id.clone(),
                id("missing"),
            ]))
            .await
            .unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[&second_id].hash, Some(second_hash));
        assert_eq!(client.get_all().await.unwrap().len(), 2);

        let mut cloned_client = client.clone();
        let deleted = cloned_client
            .delete(first_id.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(deleted.hash, Some(first_hash));
        assert!(client.get(first_id.clone()).await.unwrap().is_none());
        assert!(client.delete(first_id).await.unwrap().is_none());
        assert_eq!(client.get_all().await.unwrap().len(), 1);
    }
}
