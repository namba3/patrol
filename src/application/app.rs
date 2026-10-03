use std::collections::{HashMap, HashSet};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocUpdateEvent {
    Changed,
    PollFailed,
    PollRecovered,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DocUpdateInfo {
    pub event: DocUpdateEvent,
    pub id: String,
    pub url: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<ConfigRepository, DataRepository, Poller> App<ConfigRepository, DataRepository, Poller>
where
    ConfigRepository: domain::ConfigRepository + Send,
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
            period: std::time::Duration::from_secs(interval_period_secs.max(1)),
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
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failed_ids = match data_repo.get_all().await {
            Ok(data) => data
                .into_iter()
                .filter_map(|(id, data)| (data.consecutive_failures > 0).then_some(id))
                .collect::<HashSet<_>>(),
            Err(error) => {
                warn!("{error}");
                HashSet::new()
            }
        };

        loop {
            match &mut limit {
                Some(0) => break,
                Some(x) => *x -= 1,
                None => (),
            }

            info!("waiting for next interval period...");
            let now = interval.tick().await;
            let deadline = now + period;

            if let Err(error) = config_repo.reload().await {
                warn!("failed to reload configuration; keeping the last valid version: {error}");
            }

            let configs = config_repo
                .get_all()
                .await
                .map_err(Error::ConfigRepositoryError)?;

            let mut rem = configs.clone();
            let mut latest_errors = HashMap::new();
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
                            latest_errors.insert(id.clone(), why.to_string());
                            continue;
                        }
                    };

                    let content = content.trim_start().trim_end();

                    if content.trim().len() <= 0 {
                        warn!("[{id}]: ignore empty content.");
                        let had_failure = failed_ids.contains(&id);
                        match data_repo.record_success(id.clone()).await {
                            Ok(()) => {
                                if had_failure {
                                    failed_ids.remove(&id);
                                    let _ = tx_doc_update.send(DocUpdateInfo {
                                        event: DocUpdateEvent::PollRecovered,
                                        id: id.to_string(),
                                        url: configs[&id].url.as_str().to_owned(),
                                        timestamp: Timestamp::now().to_string(),
                                        consecutive_failures: None,
                                        error: None,
                                    });
                                }
                            }
                            Err(error) => warn!("[{id}]: {error}"),
                        }
                        let _ = rem.remove(&id);
                        continue;
                    }

                    debug!("[{id}]:\n{}", content);

                    let hash = domain::Hash::new(content.as_bytes());

                    let update_succeeded = match data_repo.update(id.clone(), hash).await {
                        Ok(Some(timestamp)) => {
                            let _ = tx_doc_update.send(DocUpdateInfo {
                                event: DocUpdateEvent::Changed,
                                id: id.to_string(),
                                url: configs[&id].url.as_str().to_owned(),
                                timestamp: timestamp.to_string(),
                                consecutive_failures: None,
                                error: None,
                            });
                            true
                        }
                        Ok(None) => true,
                        Err(why) => {
                            warn!("[{id}]: {why}");
                            false
                        }
                    };

                    if update_succeeded && failed_ids.remove(&id) {
                        let _ = tx_doc_update.send(DocUpdateInfo {
                            event: DocUpdateEvent::PollRecovered,
                            id: id.to_string(),
                            url: configs[&id].url.as_str().to_owned(),
                            timestamp: Timestamp::now().to_string(),
                            consecutive_failures: None,
                            error: None,
                        });
                    }

                    let _ = rem.remove(&id);
                }

                retry -= 1;
            }

            for (id, _config) in rem.iter() {
                let error = latest_errors
                    .remove(id)
                    .unwrap_or_else(|| "poll did not complete before the cycle deadline".into());
                match data_repo.record_failure(id.clone(), error.clone()).await {
                    Ok(consecutive_failures) => {
                        if failed_ids.insert(id.clone()) {
                            let _ = tx_doc_update.send(DocUpdateInfo {
                                event: DocUpdateEvent::PollFailed,
                                id: id.to_string(),
                                url: configs[id].url.as_str().to_owned(),
                                timestamp: Timestamp::now().to_string(),
                                consecutive_failures: Some(consecutive_failures),
                                error: Some(error),
                            });
                        }
                    }
                    Err(why) => warn!("[{id}]: failed to store poll failure: {why}"),
                }
            }

            let configured_ids = configs.keys().cloned().collect();
            let data_map = data_repo.get_multiple(configured_ids).await;
            let data_map = match data_map {
                Ok(x) => x,
                Err(why) => {
                    warn!("{why}");
                    continue;
                }
            };
            let mut data_list: Vec<_> = configs
                .iter()
                .map(|(id, _)| (id, data_map.get(id)))
                .collect();
            data_list.sort_by_key(|(_, data)| data.and_then(|data| data.last_updated));

            let now = Timestamp::now();
            let yesterday_now = now - Duration::from_days(1);
            let one_hour_ago = now - Duration::from_hours(1);

            let mut table = Table::new();

            table.add_row(row!["name", "status", "last_updated", "url",]);
            for (id, data) in data_list {
                let config = &configs[id];
                let status = match data {
                    Some(data) if data.consecutive_failures > 0 => {
                        format!("failed ({})", data.consecutive_failures)
                    }
                    Some(data) if data.last_success.is_some() => "ok".to_owned(),
                    _ => "not checked".to_owned(),
                };
                let last_updated = data.and_then(|data| data.last_updated);
                let color = match last_updated {
                    Some(t) if one_hour_ago < t => color::BRIGHT_GREEN,
                    Some(t) if yesterday_now < t => color::BRIGHT_YELLOW,
                    _ => color::BRIGHT_BLACK,
                };
                table.add_row(Row::new(vec![
                    Cell::new(id.as_str()).with_style(Attr::ForegroundColor(color)),
                    Cell::new(&status),
                    Cell::new(
                        &last_updated
                            .map(|time| time.to_string())
                            .unwrap_or_else(|| "-".to_owned()),
                    )
                    .with_style(Attr::ForegroundColor(color)),
                    Cell::new(config.url.as_str()).with_style(Attr::ForegroundColor(color)),
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            Error::ConfigRepositoryError(error) => {
                write!(f, "configuration repository error: {error}")
            }
            Error::DataRepositoryError(error) => write!(f, "data repository error: {error}"),
            Error::PollerError(error) => write!(f, "poller error: {error}"),
        }
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
    use std::{
        collections::{HashMap, HashSet},
        fmt::Display,
        path::PathBuf,
        pin::Pin,
    };

    use futures_util::{stream, Stream};

    use crate::{
        application::App,
        domain::{Config, ConfigRepository, Data, DataRepository, Hash, Id, Poller, Timestamp},
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

    struct FailingConfigRepository;

    #[async_trait::async_trait]
    impl ConfigRepository for FailingConfigRepository {
        type Error = TestPollerError;

        async fn get_all(&mut self) -> Result<HashMap<Id, Config>, Self::Error> {
            Err(TestPollerError)
        }

        async fn update(&mut self, _id: Id, _config: Config) -> Result<(), Self::Error> {
            Err(TestPollerError)
        }

        async fn delete(&mut self, _id: Id) -> Result<Option<Config>, Self::Error> {
            Err(TestPollerError)
        }
    }

    struct ReloadingConfigRepository {
        id: Id,
        config: Config,
        next_config: Config,
    }

    #[async_trait::async_trait]
    impl ConfigRepository for ReloadingConfigRepository {
        type Error = TestPollerError;

        async fn reload(&mut self) -> Result<(), Self::Error> {
            self.config = self.next_config.clone();
            Ok(())
        }

        async fn get_all(&mut self) -> Result<HashMap<Id, Config>, Self::Error> {
            Ok(HashMap::from([(self.id.clone(), self.config.clone())]))
        }

        async fn update(&mut self, _id: Id, _config: Config) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn delete(&mut self, _id: Id) -> Result<Option<Config>, Self::Error> {
            Ok(None)
        }
    }

    struct FailingUpdateRepository;

    #[async_trait::async_trait]
    impl DataRepository for FailingUpdateRepository {
        type Error = TestPollerError;

        async fn get(&mut self, _id: Id) -> Result<Option<Data>, Self::Error> {
            Ok(None)
        }

        async fn get_multiple(
            &mut self,
            _ids: HashSet<Id>,
        ) -> Result<HashMap<Id, Data>, Self::Error> {
            Ok(HashMap::new())
        }

        async fn get_all(&mut self) -> Result<HashMap<Id, Data>, Self::Error> {
            Ok(HashMap::new())
        }

        async fn update(&mut self, _id: Id, _hash: Hash) -> Result<Option<Timestamp>, Self::Error> {
            Err(TestPollerError)
        }

        async fn record_success(&mut self, _id: Id) -> Result<(), Self::Error> {
            Err(TestPollerError)
        }

        async fn record_failure(&mut self, _id: Id, _error: String) -> Result<u32, Self::Error> {
            Err(TestPollerError)
        }

        async fn update_multiple(&mut self, _map: HashMap<Id, Hash>) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn delete(&mut self, _id: Id) -> Result<Option<Data>, Self::Error> {
            Ok(None)
        }
    }

    #[test]
    fn application_errors_have_readable_display_messages() {
        type AppError = super::Error<TestPollerError, TestPollerError, TestPollerError>;

        let config_error = AppError::ConfigRepositoryError(TestPollerError);
        let data_error = AppError::DataRepositoryError(TestPollerError);
        let poller_error = AppError::PollerError(TestPollerError);

        assert_eq!(
            config_error.to_string(),
            "configuration repository error: test poller error"
        );
        assert_eq!(
            data_error.to_string(),
            "data repository error: test poller error"
        );
        assert_eq!(poller_error.to_string(), "poller error: test poller error");
    }

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
    async fn run_clamps_zero_interval_and_returns_config_repository_errors() {
        let data_path = temp_file("config-error-data");
        let data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        let app = App::new(
            FailingConfigRepository,
            data_repo,
            StaticPoller {
                contents: HashMap::new(),
            },
            0,
            Some(1),
        );
        assert_eq!(app.period, std::time::Duration::from_secs(1));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = app.run(tx).await;

        assert!(matches!(
            result,
            Err(super::Error::ConfigRepositoryError(_))
        ));
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn run_uses_configuration_reloaded_at_cycle_start() {
        let data_path = temp_file("reload-config-data");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = |url: &str| Config {
            url: crate::domain::Url::new(url.to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
        };
        let config_repo = ReloadingConfigRepository {
            id: id.clone(),
            config: config("https://example.com/old"),
            next_config: config("https://example.com/new"),
        };
        let data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        let poller = StaticPoller {
            contents: HashMap::from([(id, Ok("content".to_owned()))]),
        };
        let app = App::new(config_repo, data_repo, poller, 60, Some(1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        app.run(tx).await.unwrap();

        let update = rx.try_recv().unwrap();
        assert_eq!(update.event, super::DocUpdateEvent::Changed);
        assert_eq!(update.url, "https://example.com/new");

        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn run_does_not_notify_when_persisting_a_change_fails() {
        let config_path = temp_file("update-error-config");
        std::fs::write(
            &config_path,
            "[Page]\nurl = \"https://example.com/page\"\nselector = \"main\"\nmode = \"simple\"\n",
        )
        .unwrap();
        let config_repo = TomlConfigRepository::new(config_path.to_str().unwrap())
            .await
            .unwrap();
        let id = Id::try_from("Page".to_owned()).unwrap();
        let app = App::new(
            config_repo,
            FailingUpdateRepository,
            StaticPoller {
                contents: HashMap::from([(id, Ok("new content".to_owned()))]),
            },
            60,
            Some(1),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        app.run(tx).await.unwrap();

        assert!(rx.try_recv().is_err());
        std::fs::remove_file(config_path).unwrap();
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
        assert_eq!(update.event, super::DocUpdateEvent::Changed);
        assert_eq!(update.id, "ChangedPage");
        assert_eq!(update.url, "https://example.com/changed");
        assert!(rx.try_recv().is_err());

        let mut data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let changed = data_repo.get(changed_id).await.unwrap().unwrap();
        assert_eq!(changed.hash, Some(Hash::new("updated text")));
        assert!(changed.last_updated.is_some());
        let empty = data_repo.get(empty_id).await.unwrap().unwrap();
        assert_eq!(empty.hash, None);
        assert_eq!(empty.last_checked, None);
        assert!(empty.last_success.is_some());

        drop(data_repo);
        std::fs::remove_file(config_path).unwrap();
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn run_does_not_notify_when_content_is_unchanged() {
        let config_path = temp_file("unchanged-config");
        let data_path = temp_file("unchanged-data");
        std::fs::write(
            &config_path,
            "[Page]\nurl = \"https://example.com/page\"\nselector = \"main\"\nmode = \"simple\"\n",
        )
        .unwrap();

        let config_path_string = config_path.to_str().unwrap();
        let data_path_string = data_path.to_str().unwrap();
        let id = Id::try_from("Page".to_owned()).unwrap();
        let hash = Hash::new("same content");

        let mut seed_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        assert!(seed_repo
            .update(id.clone(), hash.clone())
            .await
            .unwrap()
            .is_some());
        drop(seed_repo);

        let mut before_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let before = before_repo.get(id.clone()).await.unwrap().unwrap();
        drop(before_repo);

        let config_repo = TomlConfigRepository::new(config_path_string).await.unwrap();
        let data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let poller = StaticPoller {
            contents: HashMap::from([(id.clone(), Ok(" same content ".to_owned()))]),
        };
        let app = App::new(config_repo, data_repo, poller, 60, Some(1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        app.run(tx).await.unwrap();

        assert!(rx.try_recv().is_err());

        let mut after_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let after = after_repo.get(id).await.unwrap().unwrap();
        assert_eq!(after.hash, Some(hash));
        assert_eq!(after.last_updated, before.last_updated);
        assert!(after.last_checked >= before.last_checked);

        drop(after_repo);
        std::fs::remove_file(config_path).unwrap();
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn run_persists_failures_and_notifies_recovery() {
        let config_path = temp_file("failed-config");
        let data_path = temp_file("failed-data");
        std::fs::write(
            &config_path,
            "[FailedPage]\nurl = \"https://example.com/failed\"\nselector = \"main\"\nmode = \"simple\"\n",
        )
        .unwrap();

        let config_path_string = config_path.to_str().unwrap();
        let data_path_string = data_path.to_str().unwrap();
        let id = Id::try_from("FailedPage".to_owned()).unwrap();
        let config_repo = TomlConfigRepository::new(config_path_string).await.unwrap();
        let data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let poller = StaticPoller {
            contents: HashMap::from([(id.clone(), Err(TestPollerError))]),
        };
        let app = App::new(config_repo, data_repo, poller, 60, Some(1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        app.run(tx).await.unwrap();

        let failure = rx.try_recv().unwrap();
        assert_eq!(failure.event, super::DocUpdateEvent::PollFailed);
        assert_eq!(failure.id, "FailedPage");
        assert_eq!(failure.consecutive_failures, Some(1));
        assert_eq!(failure.error.as_deref(), Some("test poller error"));
        assert!(rx.try_recv().is_err());
        let mut data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let failure_state = data_repo.get(id.clone()).await.unwrap().unwrap();
        assert_eq!(failure_state.hash, None);
        assert_eq!(failure_state.consecutive_failures, 1);
        assert_eq!(
            failure_state.last_error.as_deref(),
            Some("test poller error")
        );
        drop(data_repo);

        let config_repo = TomlConfigRepository::new(config_path_string).await.unwrap();
        let data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let poller = StaticPoller {
            contents: HashMap::from([(id.clone(), Ok("recovered content".to_owned()))]),
        };
        let app = App::new(config_repo, data_repo, poller, 60, Some(1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        app.run(tx).await.unwrap();

        let events = [rx.try_recv().unwrap(), rx.try_recv().unwrap()];
        assert!(events
            .iter()
            .any(|event| event.event == super::DocUpdateEvent::Changed));
        assert!(events
            .iter()
            .any(|event| event.event == super::DocUpdateEvent::PollRecovered));
        assert!(rx.try_recv().is_err());

        let mut data_repo = TomlDataRepository::new(data_path_string).await.unwrap();
        let recovered_state = data_repo.get(id).await.unwrap().unwrap();
        assert_eq!(recovered_state.consecutive_failures, 0);
        assert_eq!(recovered_state.last_error, None);

        drop(data_repo);
        std::fs::remove_file(config_path).unwrap();
        std::fs::remove_file(data_path).unwrap();
    }
}
