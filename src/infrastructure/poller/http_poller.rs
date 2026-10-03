use std::collections::HashMap;
use std::fmt::Display;

use futures_util::{stream, Stream, StreamExt};
use reqwest::Client;
use scraper::Html;

use crate::domain::{Config, Id, Poller};

#[derive(Debug)]
pub struct HttpPoller {
    client: Client,
    max_concurrent_requests: usize,
}

impl HttpPoller {
    pub fn new(max_concurrent_requests: usize) -> Self {
        let client = Client::new();
        Self {
            client,
            max_concurrent_requests: max_concurrent_requests.max(1),
        }
    }
}

#[async_trait::async_trait]
impl Poller for HttpPoller {
    type Error = Error;
    type Stream = impl Stream<Item = (Id, Result<String, Self::Error>)>;

    async fn poll(&mut self, _id: Id, config: Config) -> Result<String, Self::Error> {
        poll(&self.client, config).await
    }

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
        let client = self.client.clone();
        let requests = stream::iter(configs.into_iter().map(move |(id, config)| {
            let client = client.clone();
            async move { (id, poll(&client, config).await) }
        }));

        requests.buffer_unordered(self.max_concurrent_requests)
    }
}

async fn poll(client: &Client, config: Config) -> Result<String, Error> {
    let Config { url, selector, .. } = config;

    let response = client.get(url.as_str()).send().await?.error_for_status()?;
    let txt = response.text().await?;
    let selector: String = selector.into();

    tokio::task::spawn_blocking(move || extract_text(&txt, &selector))
        .await
        .map_err(Error::HtmlExtraction)
}

#[derive(Debug)]
pub enum Error {
    Request(reqwest::Error),
    HtmlExtraction(tokio::task::JoinError),
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(f, "HTTP request failed: {error}"),
            Self::HtmlExtraction(error) => write!(f, "HTML extraction task failed: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            Self::HtmlExtraction(error) => Some(error),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

fn extract_text(html: &str, selector: &str) -> String {
    let doc = Html::parse_document(html);
    let selector = scraper::Selector::parse(selector).unwrap();

    let content = doc
        .select(&selector)
        .flat_map(|x| x.text())
        .map(|x| x.trim_start().trim_end())
        .filter(|x| 0 < x.len())
        .collect::<Vec<_>>()
        .join("\n");

    content
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use axum::{extract::State, http::StatusCode, routing::get, Router};
    use futures_util::StreamExt;

    use super::{extract_text, Error, HttpPoller};
    use crate::domain::{Config, Id, Mode, Poller, Selector, Url};

    #[derive(Clone)]
    struct RequestTracker {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    async fn delayed_response(State(tracker): State<RequestTracker>) -> &'static str {
        let active = tracker.active.fetch_add(1, Ordering::SeqCst) + 1;
        tracker.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        tracker.active.fetch_sub(1, Ordering::SeqCst);
        "<p>response</p>"
    }

    async fn unavailable_response() -> (StatusCode, &'static str) {
        (StatusCode::SERVICE_UNAVAILABLE, "temporarily unavailable")
    }

    #[test]
    fn extracts_trimmed_text_from_each_matching_element() {
        let html = r#"
<main>
  <p class="selected"> first </p>
  <p class="selected"> second <b> nested </b></p>
  <p>ignore this</p>
</main>
"#;

        assert_eq!(extract_text(html, "p.selected"), "first\nsecond\nnested");
    }

    #[test]
    fn returns_empty_text_when_selector_has_no_matches() {
        assert_eq!(extract_text("<main><p>content</p></main>", ".missing"), "");
    }

    #[tokio::test]
    async fn limits_concurrent_http_requests() {
        let tracker = RequestTracker {
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/{_page}", get(delayed_response))
            .with_state(tracker.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let configs = (0..4)
            .map(|index| {
                (
                    Id::try_from(format!("Page{index}")).unwrap(),
                    Config {
                        url: Url::new(format!("http://{address}/page{index}")).unwrap(),
                        selector: Selector::new("p".to_owned()).unwrap(),
                        mode: Mode::Simple,
                        wait_seconds: None,
                    },
                )
            })
            .collect();
        let mut poller = HttpPoller::new(2);
        let results = poller
            .poll_multiple(configs)
            .await
            .collect::<Vec<_>>()
            .await;

        assert_eq!(results.len(), 4);
        assert!(results
            .iter()
            .all(|(_, result)| matches!(result.as_deref(), Ok("response"))));
        assert_eq!(tracker.max_active.load(Ordering::SeqCst), 2);

        server.abort();
    }

    #[tokio::test]
    async fn treats_server_error_status_as_a_poll_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/unavailable", get(unavailable_response));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let config = Config {
            url: Url::new(format!("http://{address}/unavailable")).unwrap(),
            selector: Selector::new("body".to_owned()).unwrap(),
            mode: Mode::Simple,
            wait_seconds: None,
        };
        let mut poller = HttpPoller::new(1);

        let error = poller
            .poll(Id::try_from("UnavailablePage".to_owned()).unwrap(), config)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            Error::Request(error) if error.status().map(|status| status.as_u16()) == Some(503)
        ));
        server.abort();
    }
}
