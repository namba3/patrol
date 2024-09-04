use futures_util::StreamExt;
use log::{debug, info, warn};
use prettytable::{color, row, Attr, Cell, Row, Table};
use tokio::sync::mpsc;

use crate::domain::{self, Duration, Timestamp};

pub struct App<ConfigRepository, DataRepository, Poller> {
    config_repo: ConfigRepository,
    data_repo: DataRepository,
    poller: Poller,
    period: std::time::Duration,
    limit: Option<u8>,
}

pub struct DocUpdateInfo {
    pub id: String,
    pub url: String,
    pub timestamp: String,
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
        tx_doc_update: mpsc::UnboundedSender<DocUpdateInfo>,
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
            let now = interval.tick().await;
            let deadline = now + period;

            let configs = config_repo
                .get_all()
                .await
                .map_err(Error::ConfigRepositoryError)?;

            let mut rem = configs.clone();
            let mut retry = 3;

            while 0 < rem.len() && 0 < retry {
                let poll_stream = poller.poll_multiple(rem.clone()).await;
                tokio::pin!(poll_stream);

                while let Ok(Some((id, result))) =
                    tokio::time::timeout_at(deadline, poll_stream.next()).await
                {
                    let content = match result {
                        Ok(x) => x,
                        Err(why) => {
                            warn!("[{id}]: {why}");
                            continue;
                        }
                    };

                    let content = content.trim_start().trim_end();

                    if content.trim().len() <= 0 {
                        warn!("[{id}]: ignore empty content.");
                        continue;
                    }

                    debug!("[{id}]:\n{}", content);

                    let hash = domain::Hash::new(content.as_bytes());

                    match data_repo.update(id.clone(), hash).await {
                        Ok(Some(timestamp)) => {
                            let _ = tx_doc_update.send(DocUpdateInfo {
                                id: id.to_string(),
                                url: configs[&id].url.as_str().to_owned(),
                                timestamp: timestamp.to_string(),
                            });
                        }
                        Ok(None) => (),
                        Err(why) => {
                            warn!("[{id}]: {why}")
                        }
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

            let data_list: Vec<_> = data_list
                .iter()
                .filter_map(|x| x.1.last_updated.map(|l| (x.0.clone(), l)))
                .collect();

            let mut table = Table::new();

            table.add_row(row!["name", "last_updated", "url",]);
            for (id, time) in data_list {
                let url = &configs[&id].url;
                let color = match time {
                    t if one_hour_ago < t => color::BRIGHT_GREEN,
                    t if yesterday_now < t => color::BRIGHT_YELLOW,
                    _ => color::BRIGHT_BLACK,
                };
                table.add_row(Row::new(vec![
                    Cell::new(id.as_str()).with_style(Attr::ForegroundColor(color)),
                    Cell::new(&time.to_string()).with_style(Attr::ForegroundColor(color)),
                    Cell::new(url.as_str()).with_style(Attr::ForegroundColor(color)),
                ]));
            }

            table.printstd();
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
