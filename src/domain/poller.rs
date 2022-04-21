use std::collections::HashMap;

use futures_util::stream::Stream;

use crate::domain::{Config, Id};

#[async_trait::async_trait]
pub trait Poller {
    type Error: std::error::Error + Send;
    type Stream: Stream<Item = (Id, Result<String, Self::Error>)>;

    async fn poll(&mut self, id: Id, config: Config) -> Result<String, Self::Error>;

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream;
}
