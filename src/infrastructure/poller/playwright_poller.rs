use std::{collections::HashMap, fmt::Display, sync::Arc, task::Context};

use futures::{FutureExt, TryFutureExt};
use futures_util::Stream;
use log::debug;
use playwright::{
    api::{Browser, BrowserContext, BrowserType, Page},
    Playwright,
};

use crate::domain::{Config, Id, Poller};

use tokio::sync::OnceCell;

static PLAYWRIGHT: OnceCell<Playwright> = OnceCell::const_new();

#[derive(Debug)]
pub struct PlaywrightPoller {
    _pool_size: u8,
    client_pool: ClientPool,
}

impl PlaywrightPoller {
    pub async fn new(pool_size: u8) -> Result<Self, Error> {
        let client_pool = ClientPool::new(pool_size).await?;
        Ok(Self {
            _pool_size: pool_size,
            client_pool,
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
        let mut item = self.client_pool.get().await;
        let client = item.client();

        let result = poll(client, url.as_str(), selector.as_str(), wait_seconds).await;

        // This prevents the browser from spinning and wasting CPU resources
        let _ = client.goto_builder("about:blank").goto().await;
        result
    }

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        for (id, config) in configs.into_iter() {
            let mut client_pool = self.client_pool.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
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
                    .map_err(|e| Error::from(e));

                // This prevents the browser from spinning and wasting CPU resources
                let _ = client.goto_builder("about:blank").goto().await;

                debug!("[{}]: polling succeeded", &id);
                let _ = tx.send((id, result));
            });
        }
        drop(tx);

        async_stream::stream! {
            while let Some(x) = rx.recv().await {
                yield x;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ClientPool {
    lending_tabs: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Page>>>,
    returning_tabs: tokio::sync::mpsc::UnboundedSender<Page>,
    // browser_type: BrowserType,
    // browser: Arc<Browser>,
    context: Arc<BrowserContext>,
}
impl ClientPool {
    async fn new(pool_size: u8) -> Result<Self, Error> {
        let (returning_tabs, lending_tabs) = tokio::sync::mpsc::unbounded_channel();

        let playwright = PLAYWRIGHT
            .get_or_init(async || Playwright::initialize().await.unwrap())
            .await;

        playwright.prepare()?; // Install browsers
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
            //            browser_type: chromium,
            // browser: Arc::new(browser),
            context: Arc::new(context),
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
