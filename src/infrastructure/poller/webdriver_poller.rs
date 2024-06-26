use std::{collections::HashMap, fmt::Display};

use fantoccini::{Client, ClientBuilder, Locator};
use futures_util::Stream;
use log::debug;

use crate::domain::{Config, Id, Poller};

// use std::lazy::SyncLazy;
use once_cell::sync::Lazy as SyncLazy;
use serde_json::{json, Map, Value};

static CAPABILITIES: SyncLazy<Map<String, Value>> = SyncLazy::new(|| {
    let capabilities = json!({
        "goog:chromeOptions": {
            "args": ["--headless", "--disable-extensions", "--disable-gpu"],
        },
        "moz:firefoxOptions": {
            "args": ["--headless" /* , "--safe-mode" */ ] ,
        },
        "timeouts": {
            "implicit": 30000
        }
    });

    if let Value::Object(x) = capabilities {
        x
    } else {
        unreachable!()
    }
});

#[derive(Debug)]
pub struct WebDriverPoller {
    _ports: Vec<u16>,
    client_pool: ClientPool,
}

impl WebDriverPoller {
    pub async fn new(ports: &[u16]) -> Result<Self, Error> {
        let client_pool = ClientPool::new(ports).await?;
        Ok(Self {
            _ports: ports.to_vec(),
            client_pool,
        })
    }
}

#[async_trait::async_trait]
impl Poller for WebDriverPoller {
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
        let _ = client.goto("about:blank").await;

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
                let _ = client.goto("about:blank").await;

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
    lending_port: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Client>>>,
    returning_port: tokio::sync::mpsc::UnboundedSender<Client>,
}
impl ClientPool {
    async fn new(ports: &[u16]) -> Result<Self, fantoccini::error::NewSessionError> {
        let (returning_port, lending_port) = tokio::sync::mpsc::unbounded_channel();
        let r = Self {
            lending_port: std::sync::Arc::new(tokio::sync::Mutex::new(lending_port)),
            returning_port,
        };

        for port in ports.into_iter() {
            let c = connect(*port).await?;
            debug!("webdriver connected to {port}.");
            let _ = r.returning_port.send(c);
        }

        Ok(r)
    }

    async fn get(&mut self) -> PoolItem {
        let client = self.lending_port.lock().await.recv().await.unwrap();
        PoolItem {
            client: client.into(),
            returning_port: self.returning_port.clone(),
        }
    }
}
#[derive(Debug)]
struct PoolItem {
    client: Option<Client>,
    returning_port: tokio::sync::mpsc::UnboundedSender<Client>,
}
impl PoolItem {
    pub fn client(&mut self) -> &mut Client {
        self.client.as_mut().unwrap()
    }
}
impl Drop for PoolItem {
    fn drop(&mut self) {
        let _ = self.returning_port.send(self.client.take().unwrap());
    }
}

async fn connect(port: u16) -> Result<Client, fantoccini::error::NewSessionError> {
    ClientBuilder::rustls()
        .capabilities(CAPABILITIES.clone())
        .connect(&format!("http://localhost:{}", port))
        .await
}

async fn poll(
    client: &mut Client,
    url: &str,
    selector: &str,
    wait_seconds: Option<u16>,
) -> Result<String, Error> {
    client.goto(url).await?;
    client.wait().for_element(Locator::Css("html")).await?;

    let fut = client.wait().for_element(Locator::Css(selector));

    match wait_seconds {
        Some(x) if 0 < x => tokio::time::sleep(std::time::Duration::from_secs(x as u64)).await,
        _ => (),
    }

    let timeout = std::time::Duration::from_secs(30 as u64);
    let mut elem = tokio::time::timeout(timeout, fut).await??;
    let content = elem.text().await?;

    Ok(content)
}

#[derive(Debug)]
pub enum Error {
    NewSessionError(fantoccini::error::NewSessionError),
    CmdError(fantoccini::error::CmdError),
    Timeout(tokio::time::error::Elapsed),
}
impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NewSessionError(e) => {
                f.write_fmt(format_args!("failed to establish a new connection: {e}"))
            }
            Error::CmdError(e) => {
                f.write_fmt(format_args!("failed to manipulate the browser: {e}"))
            }
            Error::Timeout(_) => f.write_fmt(format_args!("timeout")),
        }
    }
}
impl std::error::Error for Error {}
impl From<fantoccini::error::NewSessionError> for Error {
    fn from(e: fantoccini::error::NewSessionError) -> Self {
        Error::NewSessionError(e)
    }
}
impl From<fantoccini::error::CmdError> for Error {
    fn from(e: fantoccini::error::CmdError) -> Self {
        Error::CmdError(e)
    }
}
impl From<tokio::time::error::Elapsed> for Error {
    fn from(e: tokio::time::error::Elapsed) -> Self {
        Error::Timeout(e)
    }
}
