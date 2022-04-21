use futures_util::StreamExt;
use log::{info, warn};

use crate::domain::{self, Duration, Timestamp};

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
            mut data_repo,
            mut config_repo,
            mut poller,
            period,
        } = self;

        let mut interval = tokio::time::interval(period);

        loop {
            let _ = interval.tick().await;

            let configs = config_repo
                .get_all()
                .await
                .map_err(Error::ConfigRepositoryError)?;

            let poll_stream = poller.poll_multiple(configs).await;
            tokio::pin!(poll_stream);

            while let Some((id, result)) = poll_stream.next().await {
                let content = match result {
                    Ok(x) => x,
                    Err(why) => {
                        warn!("[{id}]: {why}");
                        continue;
                    }
                };

                if let Err(why) = data_repo.update(id.clone(), content).await {
                    warn!("[{id}]: {why}")
                }
            }

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

            let now = Timestamp::now();
            let yesterday_now = now - Duration::from_days(1);
            let one_hour_ago = now - Duration::from_hours(1);

            for (key, last_updated) in data_list
                .into_iter()
                .filter_map(|x| x.1.last_updated.map(|l| (x.0, l)))
            {
                let style = match last_updated {
                    _ if one_hour_ago < last_updated => ansi_term::Color::Fixed(15).bold(),
                    _ if yesterday_now < last_updated => ansi_term::Color::Fixed(7).normal(),
                    _ => ansi_term::Color::Fixed(8).normal(),
                };
                info!(
                    "[{key}]: last_updated: {}",
                    style.paint(&last_updated.to_string())
                );
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
