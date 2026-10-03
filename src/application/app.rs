use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use futures_util::StreamExt;
use log::{debug, info, warn};
use prettytable::{color, row, Attr, Cell, Row, Table};
use tokio::sync::{mpsc, oneshot, watch};

use crate::domain::{
    self, Duration, Timestamp, CHANGE_HISTORY_CONTENT_LIMIT_BYTES as HISTORY_CONTENT_LIMIT_BYTES,
};

const RETRY_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_millis(250);

fn retry_backoff(retry_number: u8) -> std::time::Duration {
    let multiplier = 1_u32
        .checked_shl(u32::from(retry_number.saturating_sub(1)))
        .unwrap_or(u32::MAX);
    RETRY_BACKOFF_BASE.saturating_mul(multiplier)
}

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

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DocChangeContent {
    pub id: String,
    pub timestamp_unix_ms: i64,
    pub content: String,
    pub content_truncated: bool,
}

pub struct DocChangeBatch {
    pub changes: Vec<DocChangeContent>,
    pub persisted: oneshot::Sender<bool>,
}

fn bounded_history_content(content: &str) -> (String, bool) {
    if content.len() <= HISTORY_CONTENT_LIMIT_BYTES {
        return (content.to_owned(), false);
    }
    let mut end = HISTORY_CONTENT_LIMIT_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (content[..end].to_owned(), true)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DocStatus {
    pub id: String,
    pub url: String,
    pub status: String,
    pub last_updated_unix_ms: Option<i64>,
    pub last_checked_unix_ms: Option<i64>,
    pub last_attempted_unix_ms: Option<i64>,
    pub last_success_unix_ms: Option<i64>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollStatus {
    Failed(u32),
    Ok,
    NotChecked,
}

impl PollStatus {
    fn from_data(data: Option<&domain::Data>) -> Self {
        match data {
            Some(data) if data.consecutive_failures > 0 => Self::Failed(data.consecutive_failures),
            Some(data) if data.last_success.is_some() => Self::Ok,
            _ => Self::NotChecked,
        }
    }

    fn api_label(self) -> &'static str {
        match self {
            Self::Failed(_) => "failed",
            Self::Ok => "ok",
            Self::NotChecked => "not_checked",
        }
    }

    fn terminal_label(self) -> Cow<'static, str> {
        match self {
            Self::Failed(count) => Cow::Owned(format!("failed ({count})")),
            Self::Ok => Cow::Borrowed("ok"),
            Self::NotChecked => Cow::Borrowed("not checked"),
        }
    }
}

fn sort_statuses_by_id(statuses: &mut [DocStatus]) {
    statuses.sort_unstable_by(|left, right| left.id.cmp(&right.id));
}

fn build_status_snapshot(
    configs: &HashMap<domain::Id, domain::Config>,
    data_map: &HashMap<domain::Id, domain::Data>,
) -> Vec<DocStatus> {
    let mut statuses = configs
        .iter()
        .map(|(id, config)| {
            let data = data_map.get(id);
            DocStatus {
                id: id.to_string(),
                url: config.url.as_str().to_owned(),
                status: PollStatus::from_data(data).api_label().to_owned(),
                last_updated_unix_ms: data
                    .and_then(|data| data.last_updated)
                    .map(|timestamp| timestamp.unix_millis()),
                last_checked_unix_ms: data
                    .and_then(|data| data.last_checked)
                    .map(|timestamp| timestamp.unix_millis()),
                last_attempted_unix_ms: data
                    .and_then(|data| data.last_attempted)
                    .map(|timestamp| timestamp.unix_millis()),
                last_success_unix_ms: data
                    .and_then(|data| data.last_success)
                    .map(|timestamp| timestamp.unix_millis()),
                consecutive_failures: data
                    .map(|data| data.consecutive_failures)
                    .unwrap_or_default(),
                last_error: data.and_then(|data| data.last_error.clone()),
            }
        })
        .collect::<Vec<_>>();
    sort_statuses_by_id(&mut statuses);
    statuses
}

async fn next_cycle_tick(
    interval: &mut tokio::time::Interval,
    shutdown: &mut Option<watch::Receiver<bool>>,
) -> Option<tokio::time::Instant> {
    loop {
        let Some(shutdown) = shutdown.as_mut() else {
            return Some(interval.tick().await);
        };
        if *shutdown.borrow() {
            return None;
        }
        tokio::select! {
            tick = interval.tick() => {
                if *shutdown.borrow() {
                    return None;
                }
                return Some(tick);
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return None;
                }
            }
        }
    }
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
        tx_doc_update: mpsc::Sender<DocUpdateInfo>,
    ) -> Result<(), Error<ConfigRepository::Error, DataRepository::Error, Poller::Error>> {
        self.run_with_status(tx_doc_update, watch::channel(Vec::new()).0)
            .await
    }

    pub async fn run_with_status(
        self,
        tx_doc_update: mpsc::Sender<DocUpdateInfo>,
        tx_status: watch::Sender<Vec<DocStatus>>,
    ) -> Result<(), Error<ConfigRepository::Error, DataRepository::Error, Poller::Error>> {
        self.run_with_history(tx_doc_update, tx_status, None).await
    }

    pub async fn run_with_history(
        self,
        tx_doc_update: mpsc::Sender<DocUpdateInfo>,
        tx_status: watch::Sender<Vec<DocStatus>>,
        tx_history: Option<mpsc::Sender<DocChangeBatch>>,
    ) -> Result<(), Error<ConfigRepository::Error, DataRepository::Error, Poller::Error>> {
        self.run_with_history_inner(tx_doc_update, tx_status, tx_history, None)
            .await
    }

    pub async fn run_with_history_and_shutdown(
        self,
        tx_doc_update: mpsc::Sender<DocUpdateInfo>,
        tx_status: watch::Sender<Vec<DocStatus>>,
        tx_history: Option<mpsc::Sender<DocChangeBatch>>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), Error<ConfigRepository::Error, DataRepository::Error, Poller::Error>> {
        self.run_with_history_inner(tx_doc_update, tx_status, tx_history, Some(shutdown))
            .await
    }

    async fn run_with_history_inner(
        self,
        tx_doc_update: mpsc::Sender<DocUpdateInfo>,
        tx_status: watch::Sender<Vec<DocStatus>>,
        tx_history: Option<mpsc::Sender<DocChangeBatch>>,
        mut shutdown: Option<watch::Receiver<bool>>,
    ) -> Result<(), Error<ConfigRepository::Error, DataRepository::Error, Poller::Error>> {
        let Self {
            mut data_repo,
            mut config_repo,
            mut poller,
            period,
            mut limit,
        } = self;
        let poll_all_once = limit == Some(1);

        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let startup_data = match data_repo.get_all().await {
            Ok(data) => data,
            Err(error) => {
                warn!("{error}");
                HashMap::new()
            }
        };
        let mut failed_ids = startup_data
            .iter()
            .filter_map(|(id, data)| (data.consecutive_failures > 0).then_some(id))
            .cloned()
            .collect::<HashSet<_>>();
        let mut startup_data = Some(startup_data);

        loop {
            match &mut limit {
                Some(0) => break,
                Some(x) => *x -= 1,
                None => (),
            }

            info!("waiting for next interval period...");
            let Some(now) = next_cycle_tick(&mut interval, &mut shutdown).await else {
                break;
            };
            let deadline = now + period;

            if let Err(error) = config_repo.reload().await {
                warn!("failed to reload configuration; keeping the last valid version: {error}");
            }

            let configs = config_repo
                .get_all()
                .await
                .map_err(Error::ConfigRepositoryError)?;

            if let Some(data) = startup_data.as_ref() {
                tx_status.send_replace(build_status_snapshot(&configs, data));
            }

            let has_custom_intervals = configs
                .values()
                .any(|config| config.poll_interval_minutes.is_some());
            let schedule_data = if poll_all_once || !has_custom_intervals {
                let _ = startup_data.take();
                HashMap::new()
            } else if let Some(data) = startup_data.take() {
                data.into_iter()
                    .filter(|(id, _)| configs.contains_key(id))
                    .collect()
            } else {
                let configured_ids = configs.keys().cloned().collect::<HashSet<_>>();
                match data_repo.get_multiple(configured_ids).await {
                    Ok(data) => data,
                    Err(error) => {
                        warn!("failed to read last-attempt times; polling all configured pages: {error}");
                        HashMap::new()
                    }
                }
            };
            let schedule_now = Timestamp::now().unix_secs();
            let poll_configs = configs
                .iter()
                .filter(|(id, config)| {
                    let Some(minutes) = config.poll_interval_minutes else {
                        return true;
                    };
                    if poll_all_once {
                        return true;
                    }
                    let interval_secs = u64::from(minutes.max(1)) * 60;
                    schedule_data
                        .get(*id)
                        .and_then(|data| data.last_attempted)
                        .is_none_or(|last_attempted| {
                            schedule_now
                                >= last_attempted
                                    .unix_secs()
                                    .saturating_add(interval_secs as i64)
                        })
                })
                .map(|(id, config)| (id.clone(), config.clone()))
                .collect::<HashMap<_, _>>();

            let has_poll_targets = !poll_configs.is_empty();
            let mut rem = poll_configs;
            let mut latest_errors = HashMap::new();
            let mut successful_hashes = HashMap::new();
            let mut successful_contents = HashMap::new();
            let mut empty_successes = HashSet::new();
            let mut retry = 3;
            let mut retry_number = 0;

            while !rem.is_empty() && 0 < retry && tokio::time::Instant::now() < deadline {
                let mut pending = rem.keys().cloned().collect::<HashSet<_>>();
                let poll_stream = poller.poll_multiple(std::mem::take(&mut rem)).await;
                tokio::pin!(poll_stream);

                while let Ok(Some((id, result))) =
                    tokio::time::timeout_at(deadline, poll_stream.next()).await
                {
                    let content = match result {
                        Ok(content) => {
                            pending.remove(&id);
                            latest_errors.remove(&id);
                            content
                        }
                        Err(why) => {
                            warn!("[{id}]: {why}");
                            latest_errors.insert(id.clone(), why.to_string());
                            pending.insert(id);
                            continue;
                        }
                    };

                    let content = content.trim();

                    if content.is_empty() {
                        warn!("[{id}]: ignore empty content.");
                        empty_successes.insert(id.clone());
                        continue;
                    }

                    debug!("[{id}]:\n{}", content);

                    let hash = domain::Hash::new(content.as_bytes());
                    successful_hashes.insert(id.clone(), hash);
                    if tx_history.is_some() {
                        successful_contents.insert(id.clone(), bounded_history_content(content));
                    }
                }

                for id in pending {
                    if let Some(config) = configs.get(&id) {
                        let _ = rem.insert(id, config.clone());
                    }
                }

                retry -= 1;
                if !rem.is_empty() && retry > 0 {
                    retry_number += 1;
                    let backoff = retry_backoff(retry_number);
                    if tokio::time::timeout_at(deadline, tokio::time::sleep(backoff))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }

            let failures: HashMap<domain::Id, String> = rem
                .keys()
                .map(|id| {
                    let error = latest_errors.remove(id).unwrap_or_else(|| {
                        "poll did not complete before the cycle deadline".into()
                    });
                    (id.clone(), error)
                })
                .collect();
            let mut newly_failed_errors = failures
                .iter()
                .filter(|&(id, _)| !failed_ids.contains(id))
                .map(|(id, error)| (id.clone(), error.clone()))
                .collect::<HashMap<_, _>>();
            let empty_success_ids = empty_successes.iter().cloned().collect::<Vec<_>>();
            let mut history_changes = Vec::new();
            let mut cycle_updates = Vec::new();
            match data_repo
                .record_poll_results(successful_hashes, empty_successes, failures)
                .await
            {
                Ok(batch) => {
                    for (id, timestamp) in batch.changed_at {
                        if let Some(timestamp) = timestamp {
                            if tx_history.is_some() {
                                if let Some((content, content_truncated)) =
                                    successful_contents.remove(&id)
                                {
                                    history_changes.push(DocChangeContent {
                                        id: id.to_string(),
                                        timestamp_unix_ms: timestamp.unix_millis(),
                                        content,
                                        content_truncated,
                                    });
                                }
                            }
                            cycle_updates.push(DocUpdateInfo {
                                event: DocUpdateEvent::Changed,
                                id: id.to_string(),
                                url: configs[&id].url.as_str().to_owned(),
                                timestamp: timestamp.to_string(),
                                consecutive_failures: None,
                                error: None,
                            });
                        }

                        if failed_ids.remove(&id) {
                            cycle_updates.push(DocUpdateInfo {
                                event: DocUpdateEvent::PollRecovered,
                                id: id.to_string(),
                                url: configs[&id].url.as_str().to_owned(),
                                timestamp: Timestamp::now().to_string(),
                                consecutive_failures: None,
                                error: None,
                            });
                        }
                    }

                    for id in empty_success_ids {
                        if failed_ids.remove(&id) {
                            cycle_updates.push(DocUpdateInfo {
                                event: DocUpdateEvent::PollRecovered,
                                id: id.to_string(),
                                url: configs[&id].url.as_str().to_owned(),
                                timestamp: Timestamp::now().to_string(),
                                consecutive_failures: None,
                                error: None,
                            });
                        }
                    }

                    for (id, consecutive_failures) in batch.failure_counts {
                        if failed_ids.insert(id.clone()) {
                            cycle_updates.push(DocUpdateInfo {
                                event: DocUpdateEvent::PollFailed,
                                id: id.to_string(),
                                url: configs[&id].url.as_str().to_owned(),
                                timestamp: Timestamp::now().to_string(),
                                consecutive_failures: Some(consecutive_failures),
                                error: newly_failed_errors.remove(&id),
                            });
                        }
                    }

                    if let Some(tx_history) = &tx_history {
                        if !history_changes.is_empty() {
                            let (tx_persisted, rx_persisted) = oneshot::channel();
                            if tx_history
                                .send(DocChangeBatch {
                                    changes: history_changes,
                                    persisted: tx_persisted,
                                })
                                .await
                                .is_err()
                                || !matches!(rx_persisted.await, Ok(true))
                            {
                                warn!("change history was not confirmed as saved");
                            }
                        }
                    }
                    for update in cycle_updates {
                        let _ = tx_doc_update.send(update).await;
                    }
                }
                Err(error) => warn!("failed to save polling results: {error}"),
            }

            let data_map = if has_poll_targets {
                let configured_ids = configs.keys().cloned().collect();
                data_repo.get_multiple(configured_ids).await
            } else {
                Ok(schedule_data)
            };
            let data_map = match data_map {
                Ok(x) => x,
                Err(why) => {
                    warn!("{why}");
                    continue;
                }
            };
            tx_status.send_replace(build_status_snapshot(&configs, &data_map));
            let mut data_list: Vec<_> = configs.keys().map(|id| (id, data_map.get(id))).collect();
            data_list.sort_by_key(|(_, data)| data.and_then(|data| data.last_updated));

            let now = Timestamp::now();
            let yesterday_now = now - Duration::from_days(1);
            let one_hour_ago = now - Duration::from_hours(1);

            let mut table = Table::new();

            table.add_row(row!["name", "status", "last_updated", "url",]);
            for (id, data) in data_list {
                let config = &configs[id];
                let status = PollStatus::from_data(data).terminal_label();
                let last_updated = data.and_then(|data| data.last_updated);
                let color = match last_updated {
                    Some(t) if one_hour_ago < t => color::BRIGHT_GREEN,
                    Some(t) if yesterday_now < t => color::BRIGHT_YELLOW,
                    _ => color::BRIGHT_BLACK,
                };
                table.add_row(Row::new(vec![
                    Cell::new(id.as_str()).with_style(Attr::ForegroundColor(color)),
                    Cell::new(status.as_ref()),
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
    ConfigRepositoryError: std::error::Error + 'static,
    DataRepositoryError: std::error::Error + 'static,
    PollerError: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfigRepositoryError(error) => Some(error),
            Self::DataRepositoryError(error) => Some(error),
            Self::PollerError(error) => Some(error),
        }
    }
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
    use tokio::sync::watch;

    use crate::{
        application::App,
        domain::{Config, ConfigRepository, Data, DataRepository, Hash, Id, Poller, Timestamp},
        infrastructure::{TomlConfigRepository, TomlDataRepository},
    };

    use super::{
        bounded_history_content, retry_backoff, sort_statuses_by_id, DocStatus, PollStatus,
        HISTORY_CONTENT_LIMIT_BYTES,
    };

    #[test]
    fn retry_backoff_grows_exponentially_and_saturates() {
        assert_eq!(retry_backoff(1), std::time::Duration::from_millis(250));
        assert_eq!(retry_backoff(2), std::time::Duration::from_millis(500));
        assert_eq!(retry_backoff(0), std::time::Duration::from_millis(250));
        assert_eq!(
            retry_backoff(u8::MAX),
            super::RETRY_BACKOFF_BASE.saturating_mul(u32::MAX)
        );
    }

    #[test]
    fn status_snapshots_are_sorted_by_id() {
        let status = |id: &str| DocStatus {
            id: id.to_owned(),
            url: String::new(),
            status: "not_checked".to_owned(),
            last_updated_unix_ms: None,
            last_checked_unix_ms: None,
            last_attempted_unix_ms: None,
            last_success_unix_ms: None,
            consecutive_failures: 0,
            last_error: None,
        };
        let mut statuses = vec![status("z-page"), status("a-page")];

        sort_statuses_by_id(&mut statuses);

        assert_eq!(statuses[0].id, "a-page");
        assert_eq!(statuses[1].id, "z-page");
    }

    #[test]
    fn poll_status_mapping_keeps_api_and_terminal_labels_consistent() {
        assert_eq!(PollStatus::from_data(None).api_label(), "not_checked");
        assert_eq!(PollStatus::from_data(None).terminal_label(), "not checked");

        let successful = Data {
            last_success: Some(Timestamp::now()),
            ..Data::default()
        };
        assert_eq!(PollStatus::from_data(Some(&successful)).api_label(), "ok");
        assert_eq!(
            PollStatus::from_data(Some(&successful)).terminal_label(),
            "ok"
        );

        let failed = Data {
            last_success: Some(Timestamp::now()),
            consecutive_failures: 2,
            ..Data::default()
        };
        assert_eq!(PollStatus::from_data(Some(&failed)).api_label(), "failed");
        assert_eq!(
            PollStatus::from_data(Some(&failed)).terminal_label(),
            "failed (2)"
        );
    }

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
            Err(TestPollerError)
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
            std::error::Error::source(&config_error)
                .unwrap()
                .to_string(),
            "test poller error"
        );
        assert_eq!(
            data_error.to_string(),
            "data repository error: test poller error"
        );
        assert_eq!(
            std::error::Error::source(&data_error).unwrap().to_string(),
            "test poller error"
        );
        assert_eq!(poller_error.to_string(), "poller error: test poller error");
        assert_eq!(
            std::error::Error::source(&poller_error)
                .unwrap()
                .to_string(),
            "test poller error"
        );
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

    #[derive(Debug)]
    struct CountingPoller(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl Poller for CountingPoller {
        type Error = TestPollerError;
        type Stream = Pin<Box<dyn Stream<Item = (Id, Result<String, Self::Error>)> + Send>>;

        async fn poll(&mut self, id: Id, _config: Config) -> Result<String, Self::Error> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(id.to_string())
        }

        async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
            self.0
                .fetch_add(configs.len(), std::sync::atomic::Ordering::SeqCst);
            Box::pin(stream::iter(
                configs
                    .into_keys()
                    .map(|id| (id.clone(), Ok(id.to_string()))),
            ))
        }
    }

    struct PendingPoller(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl Poller for PendingPoller {
        type Error = TestPollerError;
        type Stream = Pin<Box<dyn Stream<Item = (Id, Result<String, Self::Error>)> + Send>>;

        async fn poll(&mut self, _id: Id, _config: Config) -> Result<String, Self::Error> {
            std::future::pending().await
        }

        async fn poll_multiple(&mut self, _configs: HashMap<Id, Config>) -> Self::Stream {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(stream::pending())
        }
    }

    struct RetryTrackingPoller(std::sync::Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>);

    #[async_trait::async_trait]
    impl Poller for RetryTrackingPoller {
        type Error = TestPollerError;
        type Stream = Pin<Box<dyn Stream<Item = (Id, Result<String, Self::Error>)> + Send>>;

        async fn poll(&mut self, _id: Id, _config: Config) -> Result<String, Self::Error> {
            Err(TestPollerError)
        }

        async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
            self.0.lock().unwrap().push(tokio::time::Instant::now());
            Box::pin(stream::iter(
                configs.into_keys().map(|id| (id, Err(TestPollerError))),
            ))
        }
    }

    struct FailOncePoller(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl Poller for FailOncePoller {
        type Error = TestPollerError;
        type Stream = Pin<Box<dyn Stream<Item = (Id, Result<String, Self::Error>)> + Send>>;

        async fn poll(&mut self, _id: Id, _config: Config) -> Result<String, Self::Error> {
            Err(TestPollerError)
        }

        async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
            let attempt = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(stream::iter(configs.into_keys().map(move |id| {
                let result = if attempt == 0 {
                    Err(TestPollerError)
                } else {
                    Ok("recovered content".to_owned())
                };
                (id, result)
            })))
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
        let (tx, _rx) = tokio::sync::mpsc::channel(16);

        let result = app.run(tx).await;

        assert!(matches!(
            result,
            Err(super::Error::ConfigRepositoryError(_))
        ));
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn publishes_saved_status_before_the_first_poll_finishes() {
        let data_path = temp_file("startup-status-data");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = Config {
            url: crate::domain::Url::new("https://example.com/page".to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
        };
        let mut data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        data_repo
            .update(id.clone(), Hash::new("previous content"))
            .await
            .unwrap();
        let app = App::new(
            ReloadingConfigRepository {
                id,
                config: config.clone(),
                next_config: config,
            },
            data_repo,
            PendingPoller(std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0))),
            60,
            None,
        );
        let (tx_update, _rx_update) = tokio::sync::mpsc::channel(16);
        let (tx_status, mut rx_status) = watch::channel(Vec::new());
        let run_task = tokio::spawn(app.run_with_status(tx_update, tx_status));

        tokio::time::timeout(std::time::Duration::from_secs(1), rx_status.changed())
            .await
            .unwrap()
            .unwrap();
        let snapshot = rx_status.borrow();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].status, "ok");
        assert_eq!(snapshot[0].id, "Page");

        run_task.abort();
        let _ = run_task.await;
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn shutdown_finishes_the_current_cycle_without_starting_another() {
        let data_path = temp_file("graceful-shutdown-data");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = Config {
            url: crate::domain::Url::new("https://example.com/page".to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
        };
        let data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        let poller_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = App::new(
            ReloadingConfigRepository {
                id: id.clone(),
                config: config.clone(),
                next_config: config,
            },
            data_repo,
            PendingPoller(poller_calls.clone()),
            1,
            None,
        );
        let (tx_update, _rx_update) = tokio::sync::mpsc::channel(16);
        let (tx_status, _rx_status) = watch::channel(Vec::new());
        let (tx_shutdown, rx_shutdown) = watch::channel(false);
        let run_task = tokio::spawn(app.run_with_history_and_shutdown(
            tx_update,
            tx_status,
            None,
            rx_shutdown,
        ));

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while poller_calls.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tx_shutdown.send_replace(true);
        tokio::time::timeout(std::time::Duration::from_secs(2), run_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert_eq!(poller_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let mut data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(
            data_repo
                .get(id)
                .await
                .unwrap()
                .unwrap()
                .consecutive_failures,
            1
        );
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn run_does_not_start_retries_after_cycle_deadline() {
        let data_path = temp_file("deadline-retries-data");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = Config {
            url: crate::domain::Url::new("https://example.com/page".to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
        };
        let config_repo = ReloadingConfigRepository {
            id,
            config: config.clone(),
            next_config: config,
        };
        let data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        let poller_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let poller = PendingPoller(poller_calls.clone());
        let app = App::new(config_repo, data_repo, poller, 1, Some(1));
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        app.run(tx).await.unwrap();

        assert_eq!(poller_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn retries_failed_pages_with_backoff_before_recording_failure() {
        let data_path = temp_file("retry-backoff-data");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = Config {
            url: crate::domain::Url::new("https://example.com/page".to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
        };
        let data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = App::new(
            ReloadingConfigRepository {
                id: id.clone(),
                config: config.clone(),
                next_config: config,
            },
            data_repo,
            RetryTrackingPoller(attempts.clone()),
            5,
            Some(1),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        app.run(tx).await.unwrap();

        let attempts = attempts.lock().unwrap();
        assert_eq!(attempts.len(), 3);
        assert!(attempts[1].duration_since(attempts[0]) >= std::time::Duration::from_millis(240));
        assert!(attempts[2].duration_since(attempts[1]) >= std::time::Duration::from_millis(490));
        drop(attempts);
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn retry_success_persists_content_without_emitting_failure() {
        let data_path = temp_file("retry-success-data");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = Config {
            url: crate::domain::Url::new("https://example.com/page".to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
        };
        let data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = App::new(
            ReloadingConfigRepository {
                id: id.clone(),
                config: config.clone(),
                next_config: config,
            },
            data_repo,
            FailOncePoller(attempts.clone()),
            5,
            Some(1),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        app.run(tx).await.unwrap();

        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        let event = rx.try_recv().unwrap();
        assert_eq!(event.event, super::DocUpdateEvent::Changed);
        assert!(rx.try_recv().is_err());
        let mut data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        let data = data_repo.get(id).await.unwrap().unwrap();
        assert_eq!(data.hash, Some(Hash::new("recovered content")));
        assert_eq!(data.consecutive_failures, 0);
        assert_eq!(data.last_error, None);

        drop(data_repo);
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
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
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
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

        app.run(tx).await.unwrap();

        let update = rx.try_recv().unwrap();
        assert_eq!(update.event, super::DocUpdateEvent::Changed);
        assert_eq!(update.url, "https://example.com/new");

        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn per_target_interval_skips_recently_attempted_pages() {
        let data_path = temp_file("per-target-interval");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = Config {
            url: crate::domain::Url::new("https://example.com/page".to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: Some(30),
        };
        let mut data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        data_repo
            .update(id.clone(), Hash::new("existing content"))
            .await
            .unwrap();
        let config_repo = ReloadingConfigRepository {
            id: id.clone(),
            config: config.clone(),
            next_config: config.clone(),
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = App::new(
            config_repo,
            data_repo,
            CountingPoller(calls.clone()),
            1,
            Some(2),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (tx_status, rx_status) = tokio::sync::watch::channel(Vec::new());

        app.run_with_status(tx, tx_status).await.unwrap();

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let statuses = rx_status.borrow().clone();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].status, "ok");
        assert!(statuses[0].last_attempted_unix_ms.is_some());
        assert!(statuses[0].last_success_unix_ms.is_some());
        std::fs::remove_file(data_path).unwrap();
    }

    #[tokio::test]
    async fn once_run_polls_pages_even_when_their_interval_is_not_due() {
        let data_path = temp_file("once-ignores-target-interval");
        let id = Id::try_from("Page".to_owned()).unwrap();
        let config = Config {
            url: crate::domain::Url::new("https://example.com/page".to_owned()).unwrap(),
            selector: crate::domain::Selector::new("main".to_owned()).unwrap(),
            mode: crate::domain::Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: Some(30),
        };
        let mut data_repo = TomlDataRepository::new(data_path.to_str().unwrap())
            .await
            .unwrap();
        data_repo
            .update(id.clone(), Hash::new("existing content"))
            .await
            .unwrap();
        let config_repo = ReloadingConfigRepository {
            id,
            config: config.clone(),
            next_config: config,
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = App::new(
            config_repo,
            data_repo,
            CountingPoller(calls.clone()),
            60,
            Some(1),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(16);

        app.run(tx).await.unwrap();

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
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
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

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
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (tx_history, mut rx_history) = tokio::sync::mpsc::channel(16);

        let app_task =
            tokio::spawn(app.run_with_history(tx, watch::channel(Vec::new()).0, Some(tx_history)));

        let history = rx_history.recv().await.unwrap();
        assert!(rx.try_recv().is_err());
        history.persisted.send(true).unwrap();
        app_task.await.unwrap().unwrap();

        let update = rx.try_recv().unwrap();
        assert_eq!(update.event, super::DocUpdateEvent::Changed);
        assert_eq!(update.id, "ChangedPage");
        assert_eq!(update.url, "https://example.com/changed");
        assert!(rx.try_recv().is_err());
        assert_eq!(history.changes.len(), 1);
        assert_eq!(history.changes[0].id, "ChangedPage");
        assert_eq!(history.changes[0].content, "updated text");
        assert!(!history.changes[0].content_truncated);
        assert!(history.changes[0].timestamp_unix_ms > 0);

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

    #[test]
    fn history_copy_is_bounded_without_changing_input_content() {
        let content = "界".repeat(HISTORY_CONTENT_LIMIT_BYTES);
        let (history_content, truncated) = bounded_history_content(&content);

        assert!(truncated);
        assert_eq!(history_content.len(), HISTORY_CONTENT_LIMIT_BYTES - 1);
        assert_eq!(content.len(), HISTORY_CONTENT_LIMIT_BYTES * 3);
        assert!(history_content.is_char_boundary(history_content.len()));
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
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

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
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

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
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

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
