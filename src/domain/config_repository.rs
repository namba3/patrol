use crate::domain::Config;
use std::collections::HashMap;

#[async_trait::async_trait]
pub trait ConfigRepository {
    type Error: std::error::Error + Send;

    async fn get_all(&mut self) -> Result<HashMap<String, Config>, Self::Error>;
}
