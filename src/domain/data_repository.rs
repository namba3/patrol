use crate::domain::Data;
use std::collections::{HashMap, HashSet};

#[async_trait::async_trait]
pub trait DataRepository {
    type Error: std::error::Error + Send;

    async fn get(&mut self, key: String) -> Result<Option<Data>, Self::Error>;
    async fn get_multiple(
        &mut self,
        keys: HashSet<String>,
    ) -> Result<HashMap<String, Data>, Self::Error>;
    async fn get_all(&mut self) -> Result<HashMap<String, Data>, Self::Error>;

    async fn update(&mut self, key: String, content: String) -> Result<(), Self::Error>;
    async fn update_multiple(&mut self, map: HashMap<String, String>) -> Result<(), Self::Error>;
}
