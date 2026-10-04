use std::{collections::HashMap, pin::Pin};

use futures_util::stream::Stream;

use crate::domain::{Config, Id};

#[async_trait::async_trait]
pub trait Poller {
    type Error: std::error::Error + Send + 'static;
    type Stream: Stream<Item = (Id, Result<String, Self::Error>)> + Send + 'static;

    async fn poll(&mut self, id: Id, config: Config) -> Result<String, Self::Error>;

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream;
}

pub type PollStream<E> = Pin<Box<dyn Stream<Item = (Id, Result<String, E>)> + Send>>;
