use futures_util::StreamExt;
use log::{debug, info, warn};

use crate::domain::{self, Duration, Timestamp};

pub struct App<ConfigRepository, DataRepository, Poller> {
    config_repo: ConfigRepository,
    data_repo: DataRepository,
    poller: Poller,
    period: std::time::Duration,
    limit: Option<u8>,
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
        interval_limit: Option<u8>,
    ) -> Self {
        Self {
            config_repo,
            data_repo,
            poller,
            period: std::time::Duration::from_secs(interval_period_secs),
            limit: interval_limit,
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
            mut limit,
        } = self;

        let mut interval = tokio::time::interval(period);

        loop {
            match &mut limit {
                Some(0) => break,
                Some(x) => *x -= 1,
                None => (),
            }

            info!("waiting for next interval period...");
            let _ = interval.tick().await;

            let configs = config_repo
                .get_all()
                .await
                .map_err(Error::ConfigRepositoryError)?;

            let mut rem = configs.clone();
            let mut retry = 3;

            while 0 < rem.len() && 0 < retry {
                let poll_stream = poller.poll_multiple(rem.clone()).await;
                tokio::pin!(poll_stream);

                while let Some((id, result)) = poll_stream.next().await {
                    let content = match result {
                        Ok(x) => x,
                        Err(why) => {
                            warn!("[{id}]: {why}");
                            continue;
                        }
                    };

                    let content = content.trim_start().trim_end();

                    if content.len() <= 0 {
                        warn!("[{id}]: ignore empty content.");
                        continue;
                    }

                    debug!("[{id}]:\n{}", content);

                    let hash = domain::Hash::new(content.as_bytes());

                    if let Err(why) = data_repo.update(id.clone(), hash).await {
                        warn!("[{id}]: {why}")
                    }

                    let _ = rem.remove(&id);
                }

                retry -= 1;
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

            for (id, last_updated) in data_list
                .into_iter()
                .filter_map(|x| x.1.last_updated.map(|l| (x.0, l)))
            {
                let config = configs.get(&id);
                let url = config.map(|x| x.url.as_str()).unwrap_or("-");

                let style = match last_updated {
                    _ if one_hour_ago < last_updated => ansi_term::Color::Fixed(15).bold(),
                    _ if yesterday_now < last_updated => ansi_term::Color::Fixed(7).normal(),
                    _ => ansi_term::Color::Fixed(8).normal(),
                };
                info!(
                    "[{id}]: {}",
                    style.paint(format!("last_updated: {last_updated}, url: {url}"))
                );
            }
        }

        Ok(())
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
