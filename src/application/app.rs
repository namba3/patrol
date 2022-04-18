use futures_util::StreamExt;
use log::{info, warn};

use super::DataRepositoryActor;
use crate::domain::{self, DataRepository};

pub struct App<ConfigRepository, DataRepository, Poller> {
    config_repo: ConfigRepository,
    data_repo: DataRepository,
    poller: Poller,
    period: std::time::Duration,
}

impl<ConfigRepository, DataRepository, Poller> App<ConfigRepository, DataRepository, Poller>
where
    ConfigRepository: domain::ConfigRepository,
    DataRepository: domain::DataRepository + Send + 'static,
    Poller: domain::Poller,

    ConfigRepository::Error: std::error::Error,
    DataRepository::Error: std::error::Error,
    Poller::Error: std::error::Error,
{
    pub fn new(
        config_repo: ConfigRepository,
        data_repo: DataRepository,
        poller: Poller,
        interval_period_secs: u64,
    ) -> Self {
        Self {
            config_repo,
            data_repo,
            poller,
            period: std::time::Duration::from_secs(interval_period_secs),
        }
    }

    pub async fn run(
        self,
    ) -> Result<(), Error<ConfigRepository::Error, DataRepository::Error, Poller::Error>> {
        let Self {
            data_repo,
            mut config_repo,
            mut poller,
            period,
        } = self;

        let data_repo = DataRepositoryActor::new(data_repo);
        let mut data_repo = data_repo.start().await;
        {
            let mut data_repo = data_repo.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(period);
                loop {
                    let _ = interval.tick().await;

                    let data_map = data_repo.get_all().await;
                    let data_map = match data_map {
                        Ok(x) => x,
                        Err(why) => {
                            warn!("{why}");
                            continue;
                        }
                    };
                    let mut data_list: Vec<_> = data_map.into_iter().collect();
                    data_list.sort_by_key(|x| x.1.last_updated.clone());
                    for (key, last_updated) in data_list
                        .into_iter()
                        .filter_map(|x| x.1.last_updated.map(|l| (x.0, l)))
                    {
                        info!("[{key}]: last_updated: {last_updated}");
                    }
                }
            });
        }

        let mut interval = tokio::time::interval(period);

        loop {
            let _ = interval.tick().await;

            let configs = config_repo
                .get_all()
                .await
                .map_err(Error::ConfigRepositoryError)?;

            let poll_stream = poller.poll_multiple(configs).await;
            tokio::pin!(poll_stream);

            while let Some((key, result)) = poll_stream.next().await {
                let content = match result {
                    Ok(x) => x,
                    Err(why) => {
                        warn!("[{key}]: {why}");
                        continue;
                    }
                };

                if let Err(why) = data_repo.update(key.clone(), content).await {
                    warn!("[{key}]: {why}")
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error<ConfigRepositoryError, DataRepositoryError, PollerError>
where
    ConfigRepositoryError: std::error::Error,
    DataRepositoryError: std::error::Error,
    PollerError: std::error::Error,
{
    ConfigRepositoryError(ConfigRepositoryError),
    DataRepositoryError(DataRepositoryError),
    PollerError(PollerError),
}
impl<ConfigRepositoryError, DataRepositoryError, PollerError> std::fmt::Display
    for Error<ConfigRepositoryError, DataRepositoryError, PollerError>
where
    ConfigRepositoryError: std::error::Error,
    DataRepositoryError: std::error::Error,
    PollerError: std::error::Error,
{
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        todo!()
    }
}

impl<ConfigRepositoryError, DataRepositoryError, PollerError> std::error::Error
    for Error<ConfigRepositoryError, DataRepositoryError, PollerError>
where
    ConfigRepositoryError: std::error::Error,
    DataRepositoryError: std::error::Error,
    PollerError: std::error::Error,
{
}
