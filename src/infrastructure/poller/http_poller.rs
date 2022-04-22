use std::collections::HashMap;

use futures_util::Stream;
use reqwest::Client;
use scraper::Html;

use crate::domain::{Config, Id, Poller};

#[derive(Debug)]
pub struct HttpPoller {
    client: Client,
}

impl HttpPoller {
    pub fn new() -> Self {
        let client = Client::new();
        Self { client }
    }
}

#[async_trait::async_trait]
impl Poller for HttpPoller {
    type Error = reqwest::Error;
    type Stream = impl Stream<Item = (Id, Result<String, Self::Error>)>;

    async fn poll(&mut self, _id: Id, config: Config) -> Result<String, Self::Error> {
        poll(&self.client, config).await
    }

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        for (id, config) in configs.into_iter() {
            let client = self.client.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let result = poll(&client, config).await;
                let _ = tx.send((id, result));
            });
        }
        drop(tx);

        async_stream::stream! {
            while let Some(x) = rx.recv().await {
                yield x
            }
        }
    }
}

async fn poll(client: &Client, config: Config) -> Result<String, reqwest::Error> {
    let Config { url, selector, .. } = config;

    let response = client.get(url.as_str()).send().await?;
    let txt = response.text().await?;

    let doc = Html::parse_document(&txt);
    let selector = scraper::Selector::parse(selector.as_str()).unwrap();

    let content = doc
        .select(&selector)
        .flat_map(|x| x.text())
        .map(|x| x.trim_start().trim_end())
        .filter(|x| 0 < x.len())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(content)
}
