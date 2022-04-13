use std::{collections::HashMap, fmt::Display};

use futures_util::{Stream, StreamExt};

use crate::domain::{Config, Mode, Poller};

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
    type Stream = impl Stream<Item = (String, Result<String, Self::Error>)>;

    async fn poll_single(&mut self, key: String, config: Config) -> Result<String, Self::Error> {
        match config.mode {
            Mode::Full => {
                let result = self.full_mode_poller.poll_single(key, config).await;
                result.map_err(Error::FullModePollerError)
            }
            Mode::Simple => {
                let result = self.simple_mode_poller.poll_single(key, config).await;
                result.map_err(Error::SimpleModePollerError)
            }
        }
    }

    async fn poll_multiple(&mut self, configs: HashMap<String, Config>) -> Self::Stream {
        let mut full_mode_configs = HashMap::new();
        let mut simple_mode_configs = HashMap::new();

        for (key, config) in configs.into_iter() {
            match config.mode {
                Mode::Full => {
                    let _ = full_mode_configs.insert(key, config);
                }
                Mode::Simple => {
                    let _ = simple_mode_configs.insert(key, config);
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
                    Some((key, x)) = full_mode_stream.next() => (key, x.map_err(Error::FullModePollerError)),
                    Some((key, x)) = simple_mode_stream.next() => (key, x.map_err(Error::SimpleModePollerError)),
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
