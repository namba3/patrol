use std::{collections::HashMap, fmt::Display, sync::Arc};

use futures_util::{stream, Stream, StreamExt};
use log::debug;
use playwright::{
    api::{BrowserContext, Page},
    Playwright,
};

use crate::domain::{Config, Id, Poller};

use tokio::sync::OnceCell;

static PLAYWRIGHT: OnceCell<Playwright> = OnceCell::const_new();

#[derive(Debug)]
pub struct PlaywrightPoller {
    pool_size: u8,
    client_pool: Arc<OnceCell<Result<ClientPool, String>>>,
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
    type Stream = impl Stream<Item = (Id, Result<String, Self::Error>)>;

    async fn poll(&mut self, _id: Id, config: Config) -> Result<String, Self::Error> {
        let Config {
            url,
            selector,
            wait_seconds,
            ..
        } = config;
        let mut client_pool = get_or_initialize_pool(&self.client_pool, self.pool_size).await?;
        let mut item = client_pool.get().await;
        let client = item.client();

        let result = poll(client, url.as_str(), selector.as_str(), wait_seconds).await;

        // This prevents the browser from spinning and wasting CPU resources
        let _ = client.goto_builder("about:blank").goto().await;
        result
    }

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
        let client_pool = self.client_pool.clone();
        let page_count = self.pool_size;
        let concurrency = usize::from(page_count);
        let (tx, mut rx) = tokio::sync::mpsc::channel(concurrency);
        tokio::spawn(async move {
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
                            ..
                        } = config;
                        let mut item = client_pool.get().await;
                        let client = item.client();
                        debug!("[{}]: start polling {}", &id, url.as_str());
                        let result = poll(client, url.as_str(), selector.as_str(), wait_seconds)
                            .await
                            .map_err(Error::from);

                        // This prevents the browser from spinning and wasting CPU resources
                        let _ = client.goto_builder("about:blank").goto().await;

                        match &result {
                            Ok(_) => debug!("[{}]: polling succeeded", &id),
                            Err(error) => debug!("[{}]: polling failed: {error}", &id),
                        }
                        let _ = tx.send((id, result)).await;
                    }
                })
                .await;
        });

        async_stream::stream! {
            while let Some(result) = rx.recv().await {
                yield result;
            }
        }
    }
}

async fn get_or_initialize_pool(
    client_pool: &OnceCell<Result<ClientPool, String>>,
    pool_size: u8,
) -> Result<ClientPool, Error> {
    match client_pool
        .get_or_init(|| async {
            ClientPool::new(pool_size)
                .await
                .map_err(|error| error.to_string())
        })
        .await
    {
        Ok(client_pool) => Ok(client_pool.clone()),
        Err(error) => Err(Error::Other(error.clone())),
    }
}

#[derive(Debug, Clone)]
struct ClientPool {
    lending_tabs: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Page>>>,
    returning_tabs: tokio::sync::mpsc::UnboundedSender<Page>,
    _context: Arc<BrowserContext>,
}
impl ClientPool {
    async fn new(pool_size: u8) -> Result<Self, Error> {
        let (returning_tabs, lending_tabs) = tokio::sync::mpsc::unbounded_channel();

        let playwright = PLAYWRIGHT
            .get_or_try_init(|| async {
                Playwright::initialize()
                    .await
                    .map_err(|error| Error::PlaywrightError(Arc::new(error)))
            })
            .await?;

        playwright.install_chromium()?;
        let chromium = playwright.chromium();
        let browser = chromium.launcher().headless(true).launch().await?;
        let context = browser.context_builder().build().await?;

        for _ in 0..pool_size {
            let tab = context.new_page().await?;
            let _ = returning_tabs.send(tab);
        }

        let r = Self {
            lending_tabs: std::sync::Arc::new(tokio::sync::Mutex::new(lending_tabs)),
            returning_tabs,
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
) -> Result<String, Error> {
    let _resp = page.goto_builder(url).goto().await?.ok_or(Error::Unknown)?;

    // let _ = page
    //     .wait_for_selector_builder("html")
    //     .wait_for_selector()
    //     .await?
    //     .ok_or(Error::Unknown)?;

    let fut = page.wait_for_selector_builder(selector).wait_for_selector();

    match wait_seconds {
        Some(x) if 0 < x => tokio::time::sleep(std::time::Duration::from_secs(x as u64)).await,
        _ => (),
    }

    let timeout = std::time::Duration::from_secs(30 as u64);
    let elem = tokio::time::timeout(timeout, fut)
        .await??
        .ok_or(Error::Unknown)?;
    let content = elem.inner_text().await?;

    Ok(content)
}

#[derive(Debug)]
pub enum Error {
    IOError(std::io::Error),
    PlaywrightError(Arc<playwright::Error>),
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
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IOError(e)
    }
}

#[cfg(test)]
mod tests {
    use super::PlaywrightPoller;

    #[tokio::test]
    async fn defers_browser_startup_and_uses_at_least_one_page() {
        let poller = PlaywrightPoller::new(0).await.unwrap();

        assert_eq!(poller.pool_size, 1);
        assert!(poller.client_pool.get().is_none());
    }
}
impl From<Arc<playwright::Error>> for Error {
    fn from(e: Arc<playwright::Error>) -> Self {
        Error::PlaywrightError(e)
    }
}
impl From<tokio::time::error::Elapsed> for Error {
    fn from(e: tokio::time::error::Elapsed) -> Self {
        Error::Timeout(e)
    }
}
