use crate::domain::Data;
use std::collections::HashMap;

#[async_trait::async_trait]
pub trait DataRepository {
    type Error: std::error::Error + Send;

    async fn get_all(&mut self) -> Result<HashMap<String, Data>, Self::Error>;
    async fn update(&mut self, key: String, content: String) -> Result<(), Self::Error>;
}
