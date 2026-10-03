use std::{collections::HashMap, fmt::Display};

use futures_util::{Stream, StreamExt};

use crate::domain::{Config, Id, Mode, Poller};

use crate::domain;

#[derive(Debug)]
pub struct SelectivePoller<FullModePoller, SimpleModePoller> {
    full_mode_poller: FullModePoller,
    simple_mode_poller: SimpleModePoller,
}

impl<FullModePoller, SimpleModePoller> SelectivePoller<FullModePoller, SimpleModePoller>
where
    FullModePoller: domain::Poller + Send + Sync,
    SimpleModePoller: domain::Poller + Send + Sync,

    FullModePoller::Stream: Send,
    SimpleModePoller::Stream: Send,
{
    pub fn new(full_mode_poller: FullModePoller, simple_mode_poller: SimpleModePoller) -> Self {
        Self {
            full_mode_poller,
            simple_mode_poller,
        }
    }
}

#[async_trait::async_trait]
impl<FullModePoller, SimpleModePoller> Poller for SelectivePoller<FullModePoller, SimpleModePoller>
where
    FullModePoller: domain::Poller + Send + Sync,
    SimpleModePoller: domain::Poller + Send + Sync,

    FullModePoller::Stream: Send,
    SimpleModePoller::Stream: Send,
{
    type Error = Error<FullModePoller::Error, SimpleModePoller::Error>;
    type Stream = impl Stream<Item = (Id, Result<String, Self::Error>)>;

    async fn poll(&mut self, id: Id, config: Config) -> Result<String, Self::Error> {
        match config.mode {
            Mode::Full => {
                let result = self.full_mode_poller.poll(id, config).await;
                result.map_err(Error::FullModePollerError)
            }
            Mode::Simple => {
                let result = self.simple_mode_poller.poll(id, config).await;
                result.map_err(Error::SimpleModePollerError)
            }
        }
    }

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
        let mut full_mode_configs = HashMap::new();
        let mut simple_mode_configs = HashMap::new();

        for (id, config) in configs.into_iter() {
            match config.mode {
                Mode::Full => {
                    let _ = full_mode_configs.insert(id, config);
                }
                Mode::Simple => {
                    let _ = simple_mode_configs.insert(id, config);
                }
            }
        }

        let full_mode_stream = self.full_mode_poller.poll_multiple(full_mode_configs).await;
        let simple_mode_stream = self
            .simple_mode_poller
            .poll_multiple(simple_mode_configs)
            .await;

        async_stream::stream! {
            tokio::pin!(full_mode_stream);
            tokio::pin!(simple_mode_stream);

            loop {
                let result = tokio::select! {
                    Some((id, x)) = full_mode_stream.next() => (id, x.map_err(Error::FullModePollerError)),
                    Some((id, x)) = simple_mode_stream.next() => (id, x.map_err(Error::SimpleModePollerError)),
                    else => break,
                };

                yield result;
            }
        }
    }
}

#[derive(Debug)]
pub enum Error<FullModePollerError, SimpleModePollerError>
where
    FullModePollerError: std::error::Error,
    SimpleModePollerError: std::error::Error,
{
    FullModePollerError(FullModePollerError),
    SimpleModePollerError(SimpleModePollerError),
}
impl<FullModePollerError, SimpleModePollerError> Display
    for Error<FullModePollerError, SimpleModePollerError>
where
    FullModePollerError: std::error::Error,
    SimpleModePollerError: std::error::Error,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::FullModePollerError(e) => {
                f.write_fmt(format_args!("failed to poll the content: {e}"))
            }
            Error::SimpleModePollerError(e) => {
                f.write_fmt(format_args!("failed to poll the content: {e}"))
            }
        }
    }
}
impl<FullModePollerError, SimpleModePollerError> std::error::Error
    for Error<FullModePollerError, SimpleModePollerError>
where
    FullModePollerError: std::error::Error,
    SimpleModePollerError: std::error::Error,
{
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fmt::Display, pin::Pin};

    use futures_util::{stream, Stream, StreamExt};

    use super::{Error, SelectivePoller};
    use crate::domain::{Config, Id, Mode, Poller, Selector, Url};

    #[derive(Debug)]
    struct TestPollerError;

    impl Display for TestPollerError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("test poller error")
        }
    }

    impl std::error::Error for TestPollerError {}

    #[derive(Debug)]
    struct MockPoller {
        name: &'static str,
        fail: bool,
    }

    impl MockPoller {
        fn result(&self, id: &Id) -> Result<String, TestPollerError> {
            if self.fail {
                Err(TestPollerError)
            } else {
                Ok(format!("{}:{}", self.name, id.as_str()))
            }
        }
    }

    #[async_trait::async_trait]
    impl Poller for MockPoller {
        type Error = TestPollerError;
        type Stream = Pin<Box<dyn Stream<Item = (Id, Result<String, Self::Error>)> + Send>>;

        async fn poll(&mut self, id: Id, _config: Config) -> Result<String, Self::Error> {
            self.result(&id)
        }

        async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
            let results = configs
                .into_keys()
                .map(|id| {
                    let result = self.result(&id);
                    (id, result)
                })
                .collect::<Vec<_>>();

            Box::pin(stream::iter(results))
        }
    }

    fn id(value: &str) -> Id {
        Id::try_from(value.to_owned()).unwrap()
    }

    fn config(mode: Mode) -> Config {
        Config {
            url: Url::new("https://example.com".to_owned()).unwrap(),
            selector: Selector::new("main".to_owned()).unwrap(),
            mode,
            wait_seconds: None,
        }
    }

    fn poller() -> SelectivePoller<MockPoller, MockPoller> {
        SelectivePoller::new(
            MockPoller {
                name: "full",
                fail: false,
            },
            MockPoller {
                name: "simple",
                fail: false,
            },
        )
    }

    #[tokio::test]
    async fn poll_routes_to_the_poller_selected_by_mode() {
        let mut poller = poller();

        let full = poller
            .poll(id("full-page"), config(Mode::Full))
            .await
            .unwrap();
        let simple = poller
            .poll(id("simple-page"), config(Mode::Simple))
            .await
            .unwrap();

        assert_eq!(full, "full:full-page");
        assert_eq!(simple, "simple:simple-page");
    }

    #[tokio::test]
    async fn poll_multiple_splits_modes_and_merges_results() {
        let mut poller = poller();
        let full_id = id("full-page");
        let simple_id = id("simple-page");
        let configs = HashMap::from([
            (full_id.clone(), config(Mode::Full)),
            (simple_id.clone(), config(Mode::Simple)),
        ]);

        let results = poller
            .poll_multiple(configs)
            .await
            .collect::<Vec<_>>()
            .await;
        let results = results.into_iter().collect::<HashMap<_, _>>();

        assert_eq!(results.len(), 2);
        assert_eq!(results[&full_id].as_ref().unwrap(), "full:full-page");
        assert_eq!(results[&simple_id].as_ref().unwrap(), "simple:simple-page");
    }

    #[tokio::test]
    async fn poll_errors_identify_the_selected_poller() {
        let mut poller = SelectivePoller::new(
            MockPoller {
                name: "full",
                fail: false,
            },
            MockPoller {
                name: "simple",
                fail: true,
            },
        );

        let result = poller.poll(id("simple-page"), config(Mode::Simple)).await;

        assert!(matches!(result, Err(Error::SimpleModePollerError(_))));
    }
}
