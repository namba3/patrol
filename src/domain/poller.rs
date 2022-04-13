use std::collections::HashMap;

use futures_util::stream::Stream;

use crate::domain::Config;

#[async_trait::async_trait]
pub trait Poller {
    type Error: std::error::Error + Send;
    type Stream: Stream<Item = (String, Result<String, Self::Error>)>;

    async fn poll_single(&mut self, key: String, config: Config) -> Result<String, Self::Error>;

    async fn poll_multiple(&mut self, configs: HashMap<String, Config>) -> Self::Stream;
}
