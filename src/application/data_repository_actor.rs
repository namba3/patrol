use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use crate::domain::{self, Id};
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
        tx: oneshot::Sender<Result<(), E>>,
        id: Id,
        hash: domain::Hash,
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

    async fn update(&mut self, id: Id, hash: domain::Hash) -> Result<(), Self::Error> {
        let (tx, rx) = oneshot::channel();
        if let Err(_e) = self.tx_message.send(Message::Update { tx, id, hash }) {
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
