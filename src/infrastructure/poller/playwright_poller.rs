use std::{collections::HashMap, fmt::Display, sync::Arc};
use std::{future::Future, time::Duration};

use futures_util::{stream, Stream, StreamExt};
use log::debug;
use playwright_rs::{install_browsers, BrowserContext, Error as PlaywrightError, Page, Playwright};

use crate::domain::{Config, Id, PollStream, Poller};
use crate::infrastructure::poller::normalize_whitespace;

use tokio::sync::OnceCell;

const BROWSER_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct PlaywrightPoller {
    pool_size: u8,
    client_pool: Arc<OnceCell<ClientPool>>,
}

impl PlaywrightPoller {
    pub async fn new(pool_size: u8) -> Result<Self, Error> {
        let pool_size = pool_size.max(1);
        Ok(Self {
            pool_size,
            client_pool: Arc::new(OnceCell::new()),
        })
    }
}

#[async_trait::async_trait]
impl Poller for PlaywrightPoller {
    type Error = Error;
    type Stream = PollStream<Error>;

    async fn poll(&mut self, _id: Id, config: Config) -> Result<String, Self::Error> {
        let Config {
            url,
            selector,
            wait_seconds,
            exclude_selectors,
            normalize_whitespace: normalize,
            ..
        } = config;
        let mut client_pool = get_or_initialize_pool(&self.client_pool, self.pool_size).await?;
        let mut item = client_pool.get().await;
        let client = item.client();

        let result = poll(
            client,
            url.as_str(),
            selector.as_str(),
            wait_seconds,
            &exclude_selectors,
            normalize,
        )
        .await;

        reset_page(client).await;
        result
    }

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
        let client_pool = self.client_pool.clone();
        let page_count = self.pool_size;
        let concurrency = usize::from(page_count);
        let (tx, rx) = tokio::sync::mpsc::channel(concurrency);
        let producer = tokio::spawn(async move {
            stream::iter(configs)
                .for_each_concurrent(concurrency, |(id, config)| {
                    let client_pool = client_pool.clone();
                    let tx = tx.clone();
                    async move {
                        let mut client_pool =
                            match get_or_initialize_pool(&client_pool, page_count).await {
                                Ok(client_pool) => client_pool,
                                Err(error) => {
                                    let _ = tx.send((id, Err(error))).await;
                                    return;
                                }
                            };
                        let Config {
                            url,
                            selector,
                            wait_seconds,
                            exclude_selectors,
                            normalize_whitespace: normalize,
                            ..
                        } = config;
                        let mut item = client_pool.get().await;
                        let client = item.client();
                        debug!("[{}]: start polling {}", id, url.as_str());
                        let result = poll(
                            client,
                            url.as_str(),
                            selector.as_str(),
                            wait_seconds,
                            &exclude_selectors,
                            normalize,
                        )
                        .await;

                        reset_page(client).await;

                        match &result {
                            Ok(_) => debug!("[{}]: polling succeeded", id),
                            Err(error) => debug!("[{}]: polling failed: {error}", id),
                        }
                        let _ = tx.send((id, result)).await;
                    }
                })
                .await;
        });

        Box::pin(stream_with_producer(producer, rx))
    }
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn stream_with_producer<T, Item>(
    producer: tokio::task::JoinHandle<T>,
    mut receiver: tokio::sync::mpsc::Receiver<Item>,
) -> impl Stream<Item = Item> {
    let producer = AbortOnDrop(producer);
    async_stream::stream! {
        let _producer = producer;
        while let Some(item) = receiver.recv().await {
            yield item;
        }
    }
}

async fn get_or_initialize_pool(
    client_pool: &OnceCell<ClientPool>,
    pool_size: u8,
) -> Result<ClientPool, Error> {
    let client_pool = get_or_try_init(client_pool, || ClientPool::new(pool_size)).await?;
    Ok(client_pool.clone())
}

async fn get_or_try_init<T, E, Init, Fut>(cell: &OnceCell<T>, initialize: Init) -> Result<&T, E>
where
    Init: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    cell.get_or_try_init(initialize).await
}

#[derive(Debug, Clone)]
struct ClientPool {
    lending_tabs: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Page>>>,
    returning_tabs: tokio::sync::mpsc::UnboundedSender<Page>,
    _playwright: Arc<Playwright>,
    _context: Arc<BrowserContext>,
}
impl ClientPool {
    async fn new(pool_size: u8) -> Result<Self, Error> {
        let (returning_tabs, lending_tabs) = tokio::sync::mpsc::unbounded_channel();

        install_browsers(Some(&["chromium"])).await?;
        let playwright = Arc::new(Playwright::launch().await?);

        let chromium = playwright.chromium();
        let browser = chromium.launch().await?;
        let context = browser.new_context().await?;

        for _ in 0..pool_size {
            let tab = context.new_page().await?;
            let _ = returning_tabs.send(tab);
        }

        let r = Self {
            lending_tabs: std::sync::Arc::new(tokio::sync::Mutex::new(lending_tabs)),
            returning_tabs,
            _playwright: playwright,
            _context: Arc::new(context),
        };

        Ok(r)
    }

    async fn get(&mut self) -> PoolItem {
        let client = self.lending_tabs.lock().await.recv().await.unwrap();
        PoolItem {
            client: client.into(),
            returning_port: self.returning_tabs.clone(),
        }
    }
}
#[derive(Debug)]
struct PoolItem {
    client: Option<Page>,
    returning_port: tokio::sync::mpsc::UnboundedSender<Page>,
}
impl PoolItem {
    pub fn client(&mut self) -> &mut Page {
        self.client.as_mut().unwrap()
    }
}
impl Drop for PoolItem {
    fn drop(&mut self) {
        let _ = self.returning_port.send(self.client.take().unwrap());
    }
}

async fn poll(
    page: &mut Page,
    url: &str,
    selector: &str,
    wait_seconds: Option<u16>,
    exclude_selectors: &[crate::domain::Selector],
    normalize: bool,
) -> Result<String, Error> {
    let navigation = async { page.goto(url, None).await.map_err(Error::from) };
    let _resp = with_timeout(navigation, BROWSER_OPERATION_TIMEOUT)
        .await?
        .ok_or(Error::Unknown)?;

    // let _ = page
    //     .wait_for_selector_builder("html")
    //     .wait_for_selector()
    //     .await?
    //     .ok_or(Error::Unknown)?;

    match wait_seconds {
        Some(x) if 0 < x => tokio::time::sleep(std::time::Duration::from_secs(x as u64)).await,
        _ => (),
    }

    let selector_wait = async {
        page.locator(selector)
            .wait_for(None)
            .await
            .map_err(Error::from)
    };
    with_timeout(selector_wait, BROWSER_OPERATION_TIMEOUT).await?;
    if exclude_selectors.is_empty() {
        let content = page.locator(selector).inner_text().await?;
        return Ok(normalize_whitespace(content, normalize));
    }

    let excluded = exclude_selectors
        .iter()
        .map(|selector| selector.as_str().to_owned())
        .collect::<Vec<_>>();
    let content: String = page
        .locator(selector)
        .evaluate(
            "(element, selectors) => { const copy = element.cloneNode(true); for (const selector of selectors) copy.querySelectorAll(selector).forEach(node => node.remove()); return copy.innerText; }",
            Some(excluded),
        )
        .await?;

    Ok(normalize_whitespace(content, normalize))
}

async fn reset_page(page: &mut Page) {
    // This prevents the browser from spinning and wasting CPU resources.
    if let Err(error) = page.goto("about:blank", None).await {
        debug!("failed to reset browser page: {error}");
    }
}

async fn with_timeout<F, T>(future: F, timeout: Duration) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
{
    tokio::time::timeout(timeout, future).await?
}

#[derive(Debug)]
pub enum Error {
    IOError(std::io::Error),
    PlaywrightError(Arc<PlaywrightError>),
    Timeout(tokio::time::error::Elapsed),
    Other(String),
    Unknown,
}
impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::IOError(e) => f.write_fmt(format_args!("io error: {e}")),
            Error::PlaywrightError(e) => {
                f.write_fmt(format_args!("failed to manipulate the browser: {e}"))
            }
            Error::Timeout(_) => f.write_fmt(format_args!("timeout")),
            Error::Other(s) => f.write_fmt(format_args!("other error occurred: {s}")),
            Error::Unknown => f.write_str("unknown error occurred."),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IOError(error) => Some(error),
            Self::PlaywrightError(error) => Some(error.as_ref()),
            Self::Timeout(error) => Some(error),
            Self::Other(_) | Self::Unknown => None,
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IOError(e)
    }
}
impl From<PlaywrightError> for Error {
    fn from(error: PlaywrightError) -> Self {
        Error::PlaywrightError(Arc::new(error))
    }
}
impl From<tokio::time::error::Elapsed> for Error {
    fn from(e: tokio::time::error::Elapsed) -> Self {
        Error::Timeout(e)
    }
}

#[cfg(test)]
mod tests {
    use std::{future::pending, time::Duration};

    use tokio::sync::{oneshot, OnceCell};

    use super::{get_or_try_init, stream_with_producer, with_timeout, Error, PlaywrightPoller};

    #[test]
    fn poller_errors_expose_underlying_causes_when_available() {
        let io_error = Error::IOError(std::io::Error::other("browser process unavailable"));

        assert_eq!(
            std::error::Error::source(&io_error).unwrap().to_string(),
            "browser process unavailable"
        );
        assert!(std::error::Error::source(&Error::Unknown).is_none());
        assert!(std::error::Error::source(&Error::Other("internal".into())).is_none());
    }

    struct NotifyOnDrop(Option<oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }

    #[tokio::test]
    async fn defers_browser_startup_and_uses_at_least_one_page() {
        let poller = PlaywrightPoller::new(0).await.unwrap();

        assert_eq!(poller.pool_size, 1);
        assert!(poller.client_pool.get().is_none());
    }

    #[tokio::test]
    async fn failed_pool_initialization_can_be_retried() {
        let cell = OnceCell::new();
        let mut attempts = 0;
        let error: Result<&u8, &str> = get_or_try_init(&cell, || async {
            attempts += 1;
            Err("temporary initialization failure")
        })
        .await;

        assert_eq!(error.unwrap_err(), "temporary initialization failure");
        assert!(cell.get().is_none());

        let value = get_or_try_init(&cell, || async {
            attempts += 1;
            Ok::<_, &str>(42_u8)
        })
        .await
        .unwrap();

        assert_eq!(*value, 42);
        assert_eq!(attempts, 2);
    }

    #[tokio::test]
    async fn browser_operations_return_timeout_errors_when_they_exceed_the_limit() {
        let operation = async {
            let (): () = pending().await;
            Ok::<(), Error>(())
        };

        let result = with_timeout(operation, Duration::from_millis(1)).await;

        assert!(matches!(result, Err(Error::Timeout(_))));
    }

    #[tokio::test]
    async fn dropping_unpolled_result_stream_cancels_producer() {
        let (started_tx, started_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let (_result_tx, result_rx) = tokio::sync::mpsc::channel::<()>(1);
        let task = tokio::spawn(async move {
            let _notify = NotifyOnDrop(Some(dropped_tx));
            let _ = started_tx.send(());
            pending::<()>().await;
        });
        let result_stream = stream_with_producer(task, result_rx);

        started_rx.await.unwrap();
        drop(result_stream);

        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }
}
