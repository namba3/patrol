use crate::domain::{Data, Id};
use std::collections::{HashMap, HashSet};

#[async_trait::async_trait]
pub trait DataRepository {
    type Error: std::error::Error + Send;

    async fn get(&mut self, id: Id) -> Result<Option<Data>, Self::Error>;
    async fn get_multiple(&mut self, ids: HashSet<Id>) -> Result<HashMap<Id, Data>, Self::Error>;
    async fn get_all(&mut self) -> Result<HashMap<Id, Data>, Self::Error>;

    async fn update(&mut self, id: Id, content: String) -> Result<(), Self::Error>;
    async fn update_multiple(&mut self, map: HashMap<Id, String>) -> Result<(), Self::Error>;
}
