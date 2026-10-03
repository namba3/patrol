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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fmt::Display, path::PathBuf, pin::Pin};

    use futures_util::{stream, Stream};

    use crate::{
        application::App,
        domain::{Config, DataRepository, Hash, Id, Poller},
        infrastructure::{TomlConfigRepository, TomlDataRepository},
    };

    #[derive(Debug, Clone)]
    struct TestPollerError;

    impl Display for TestPollerError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("test poller error")
        }
    }

    impl std::error::Error for TestPollerError {}

    #[derive(Debug)]
    struct StaticPoller {
        contents: HashMap<Id, Result<String, TestPollerError>>,
    }

    #[async_trait::async_trait]
    impl Poller for StaticPoller {
        type Error = TestPollerError;
        type Stream = Pin<Box<dyn Stream<Item = (Id, Result<String, Self::Error>)> + Send>>;

        async fn poll(&mut self, id: Id, _config: Config) -> Result<String, Self::Error> {
            self.contents
                .get(&id)
                .cloned()
                .unwrap_or_else(|| Ok(String::new()))
        }

        async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
            let results = configs
                .into_keys()
                .map(|id| {
                    let result = self
                        .contents
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| Ok(String::new()));
                    (id, result)
                })
                .collect::<Vec<_>>();

            Box::pin(stream::iter(results))
        }
    }

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("patrol-app-{name}-{}.toml", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn run_notifies_non_empty_changes_and_ignores_empty_content() {
        let config_path = temp_file("config");
        let data_path = temp_file("data");
        let config_text = r#"
[ChangedPage]
url = "https://example.com/changed"
selector = "main"
mode = "simple"

[EmptyPage]
url = "https://example.com/empty"
selector = "main"
mode = "simple"
"#;
        std::fs::write(&config_path, config_text).unwrap();

        let config_path_string = config_path.to_str().unwrap();
        let data_path_string = data_path.to_str().unwrap();
        let config_repo = TomlConfigRepository::new(config_path_string).await.unwrap();
        let data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let changed_id = Id::try_from("ChangedPage".to_owned()).unwrap();
        let empty_id = Id::try_from("EmptyPage".to_owned()).unwrap();
        let poller = StaticPoller {
            contents: HashMap::from([
                (changed_id.clone(), Ok("  updated text  \n".to_owned())),
                (empty_id.clone(), Ok(" \n\t ".to_owned())),
            ]),
        };
        let app = App::new(config_repo, data_repo, poller, 60, Some(1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        app.run(tx).await.unwrap();

        let update = rx.try_recv().unwrap();
        assert_eq!(update.id, "ChangedPage");
        assert_eq!(update.url, "https://example.com/changed");
        assert!(rx.try_recv().is_err());

        let mut data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let changed = data_repo.get(changed_id).await.unwrap().unwrap();
        assert_eq!(changed.hash, Some(Hash::new("updated text")));
        assert!(changed.last_updated.is_some());
        assert!(data_repo.get(empty_id).await.unwrap().is_none());

        drop(data_repo);
        std::fs::remove_file(config_path).unwrap();
        std::fs::remove_file(data_path).unwrap();
    }
}
