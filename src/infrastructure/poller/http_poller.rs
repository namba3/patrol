use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::time::Duration;

use futures_util::{stream, StreamExt};
use reqwest::Client;
use scraper::Html;

use crate::domain::{Config, Id, PollStream, Poller};
use crate::infrastructure::poller::normalize_whitespace;

const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_RESPONSE_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const HTTP_RESPONSE_INITIAL_CAPACITY_BYTES: usize = 64 * 1024;

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
    type Stream = PollStream<Error>;

    async fn poll(&mut self, _id: Id, config: Config) -> Result<String, Self::Error> {
        poll(&self.client, config).await
    }

    async fn poll_multiple(&mut self, configs: HashMap<Id, Config>) -> Self::Stream {
        let client = self.client.clone();
        let requests = stream::iter(configs.into_iter().map(move |(id, config)| {
            let client = client.clone();
            async move { (id, poll(&client, config).await) }
        }));

        Box::pin(requests.buffer_unordered(self.max_concurrent_requests))
    }
}

async fn poll(client: &Client, config: Config) -> Result<String, Error> {
    poll_with_timeout(client, config, HTTP_REQUEST_TIMEOUT).await
}

async fn poll_with_timeout(
    client: &Client,
    config: Config,
    timeout: Duration,
) -> Result<String, Error> {
    let Config {
        url,
        selector,
        exclude_selectors,
        normalize_whitespace: normalize,
        ..
    } = config;

    let response = client
        .get(url.as_str())
        .timeout(timeout)
        .send()
        .await?
        .error_for_status()?;
    let txt = response_text(response, HTTP_RESPONSE_BODY_LIMIT_BYTES).await?;
    tokio::task::spawn_blocking(move || {
        normalize_whitespace(
            extract_text(&txt, selector.parsed(), &exclude_selectors),
            normalize,
        )
    })
    .await
    .map_err(Error::HtmlExtraction)
}

async fn response_text(response: reqwest::Response, max_bytes: usize) -> Result<String, Error> {
    let length = response.content_length();
    if length.is_some_and(|length| length > max_bytes as u64) {
        return Err(Error::ResponseBodyTooLarge { max_bytes });
    }

    let mut response = response;
    let mut body = Vec::with_capacity(initial_body_capacity(length, max_bytes));
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(Error::ResponseBodyTooLarge { max_bytes });
        }
        body.extend_from_slice(&chunk);
    }

    Ok(decode_response_body(body))
}

fn initial_body_capacity(content_length: Option<u64>, max_bytes: usize) -> usize {
    content_length
        .unwrap_or_default()
        .min(max_bytes as u64)
        .min(HTTP_RESPONSE_INITIAL_CAPACITY_BYTES as u64) as usize
}

fn decode_response_body(mut body: Vec<u8>) -> String {
    if body.starts_with(&[0xef, 0xbb, 0xbf]) {
        drop(body.drain(..3));
    }

    match String::from_utf8(body) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    }
}

#[derive(Debug)]
pub enum Error {
    Request(reqwest::Error),
    HtmlExtraction(tokio::task::JoinError),
    ResponseBodyTooLarge { max_bytes: usize },
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(f, "HTTP request failed: {error}"),
            Self::HtmlExtraction(error) => write!(f, "HTML extraction task failed: {error}"),
            Self::ResponseBodyTooLarge { max_bytes } => {
                write!(f, "HTTP response body exceeded the {max_bytes}-byte limit")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            Self::HtmlExtraction(error) => Some(error),
            Self::ResponseBodyTooLarge { .. } => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

fn extract_text(
    html: &str,
    selector: &scraper::Selector,
    exclude_selectors: &[crate::domain::Selector],
) -> String {
    let doc = Html::parse_document(html);
    let mut content = String::new();
    let mut excluded_nodes = HashSet::new();
    for element in doc.select(selector) {
        if exclude_selectors.is_empty() {
            for text in element.text() {
                append_text(&mut content, text);
            }
            continue;
        }

        excluded_nodes.clear();
        for exclude_selector in exclude_selectors {
            excluded_nodes.extend(
                element
                    .select(exclude_selector.parsed())
                    .map(|element| element.id()),
            );
        }

        if excluded_nodes.is_empty() {
            for text in element.text() {
                append_text(&mut content, text);
            }
            continue;
        }

        let mut next_node = element.first_child();
        // Defer siblings while descending so the stack stays bounded by tree depth.
        let mut pending_siblings = Vec::new();
        while let Some(node) = next_node {
            // Excluding a node also skips its entire subtree.
            if excluded_nodes.contains(&node.id()) {
                next_node = node.next_sibling().or_else(|| pending_siblings.pop());
                continue;
            }

            if let Some(text) = node.value().as_text() {
                append_text(&mut content, text);
                next_node = node.next_sibling().or_else(|| pending_siblings.pop());
                continue;
            }

            if let Some(child) = node.first_child() {
                if let Some(sibling) = node.next_sibling() {
                    pending_siblings.push(sibling);
                }
                next_node = Some(child);
            } else {
                next_node = node.next_sibling().or_else(|| pending_siblings.pop());
            }
        }
    }

    content
}

fn append_text(content: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str(text);
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use axum::{
        body::{Body, Bytes},
        extract::State,
        http::StatusCode,
        response::Response,
        routing::get,
        Router,
    };
    use futures_util::StreamExt;

    use super::{
        decode_response_body, extract_text, initial_body_capacity, poll_with_timeout,
        response_text, Error, HttpPoller, HTTP_RESPONSE_INITIAL_CAPACITY_BYTES,
    };
    use crate::domain::{Config, Id, Mode, Poller, Selector, Url};

    #[derive(Clone)]
    struct RequestTracker {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct RequestStartTracker {
        started: Arc<AtomicUsize>,
        first_request: Arc<tokio::sync::Notify>,
    }

    async fn delayed_response(State(tracker): State<RequestTracker>) -> &'static str {
        let active = tracker.active.fetch_add(1, Ordering::SeqCst) + 1;
        tracker.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        tracker.active.fetch_sub(1, Ordering::SeqCst);
        "<p>response</p>"
    }

    async fn tracked_slow_response(State(tracker): State<RequestStartTracker>) -> &'static str {
        tracker.started.fetch_add(1, Ordering::SeqCst);
        tracker.first_request.notify_one();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        "<p>response</p>"
    }

    async fn unavailable_response() -> (StatusCode, &'static str) {
        (StatusCode::SERVICE_UNAVAILABLE, "temporarily unavailable")
    }

    #[test]
    fn decodes_response_text_without_a_utf8_bom() {
        assert_eq!(decode_response_body(b"\xef\xbb\xbfhello".to_vec()), "hello");
    }

    #[test]
    fn replaces_invalid_utf8_in_response_text() {
        assert_eq!(decode_response_body(vec![b'a', 0xff, b'b']), "a�b");
    }

    #[test]
    fn caps_initial_body_allocation_even_when_content_length_is_large() {
        assert_eq!(initial_body_capacity(Some(1_024), 16 * 1024 * 1024), 1_024);
        assert_eq!(
            initial_body_capacity(Some(u64::MAX), 16 * 1024 * 1024),
            HTTP_RESPONSE_INITIAL_CAPACITY_BYTES
        );
        assert_eq!(initial_body_capacity(None, 16 * 1024 * 1024), 0);
    }

    async fn slow_response() -> &'static str {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        "too late"
    }

    async fn large_response() -> &'static str {
        "this response body is deliberately larger than the configured test limit"
    }

    async fn chunked_large_response() -> Response {
        let chunks = [
            Ok::<_, std::convert::Infallible>(Bytes::from_static(b"this response ")),
            Ok(Bytes::from_static(b"is larger than the test limit")),
        ];
        Response::new(Body::from_stream(futures_util::stream::iter(chunks)))
    }

    #[test]
    fn extracts_trimmed_text_from_each_matching_element() {
        let selector = Selector::new("p.selected".to_owned()).unwrap();
        let html = r#"
<main>
  <p class="selected"> first </p>
  <p class="selected"> second <b> nested </b></p>
  <p>ignore this</p>
</main>
"#;

        assert_eq!(
            extract_text(html, selector.parsed(), &[]),
            "first\nsecond\nnested"
        );
    }

    #[test]
    fn returns_empty_text_when_selector_has_no_matches() {
        let selector = Selector::new(".missing".to_owned()).unwrap();
        assert_eq!(
            extract_text("<main><p>content</p></main>", selector.parsed(), &[]),
            ""
        );
    }

    #[test]
    fn excludes_matching_descendants_from_selected_content() {
        let selector = Selector::new("main".to_owned()).unwrap();
        let excluded = Selector::new(".timestamp".to_owned()).unwrap();
        let html = "<main><p>Updated <time class=\"timestamp\">today</time></p><p>Body</p></main>";

        assert_eq!(
            extract_text(html, selector.parsed(), &[excluded]),
            "Updated\nBody"
        );
    }

    #[test]
    fn excludes_matching_descendants_from_each_selected_element() {
        let selector = Selector::new("article".to_owned()).unwrap();
        let excluded = Selector::new(".timestamp".to_owned()).unwrap();
        let html = "<article>First <time class=\"timestamp\">today</time></article><article>Second <time class=\"timestamp\">now</time></article>";

        assert_eq!(
            extract_text(html, selector.parsed(), &[excluded]),
            "First\nSecond"
        );
    }

    #[test]
    fn skips_nested_excluded_subtrees_and_preserves_document_order() {
        let selector = Selector::new("article".to_owned()).unwrap();
        let excluded = [
            Selector::new(".remove".to_owned()).unwrap(),
            Selector::new(".remove span".to_owned()).unwrap(),
        ];
        let html = "<article>Before <section class=\"remove\">hidden <span>nested</span></section> after <p>end</p></article><article>next</article>";

        assert_eq!(
            extract_text(html, selector.parsed(), &excluded),
            "Before\nafter\nend\nnext"
        );
    }

    #[test]
    fn uses_normal_text_traversal_when_exclusions_do_not_match() {
        let selector = Selector::new("main".to_owned()).unwrap();
        let excluded = Selector::new(".missing".to_owned()).unwrap();

        assert_eq!(
            extract_text(
                "<main>first <span>nested</span> last</main>",
                selector.parsed(),
                &[excluded],
            ),
            "first\nnested\nlast"
        );
    }

    #[test]
    fn whitespace_normalization_collapses_runs() {
        assert_eq!(
            super::super::normalize_whitespace("  one\n\t two   three  ".to_owned(), true),
            "one two three"
        );
        assert_eq!(
            super::super::normalize_whitespace("one\n two".to_owned(), false),
            "one\n two"
        );
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
                        exclude_selectors: Vec::new(),
                        normalize_whitespace: false,
                        poll_interval_minutes: None,
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
    async fn dropping_poll_stream_does_not_start_queued_requests() {
        let tracker = RequestStartTracker {
            started: Arc::new(AtomicUsize::new(0)),
            first_request: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/{_page}", get(tracked_slow_response))
            .with_state(tracker.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let configs = (0..4)
            .map(|index| {
                (
                    Id::try_from(format!("CancelPage{index}")).unwrap(),
                    Config {
                        url: Url::new(format!("http://{address}/page{index}")).unwrap(),
                        selector: Selector::new("p".to_owned()).unwrap(),
                        mode: Mode::Simple,
                        wait_seconds: None,
                        exclude_selectors: Vec::new(),
                        normalize_whitespace: false,
                        poll_interval_minutes: None,
                    },
                )
            })
            .collect();
        let mut poller = HttpPoller::new(1);
        let mut results = poller.poll_multiple(configs).await;
        {
            let mut first_result = Box::pin(results.next());
            tokio::select! {
                _ = tracker.first_request.notified() => (),
                result = &mut first_result => panic!("poll stream ended before a request started: {result:?}"),
            }
        }

        drop(results);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        assert_eq!(tracker.started.load(Ordering::SeqCst), 1);
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
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
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

    #[tokio::test]
    async fn times_out_slow_http_requests() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/slow", get(slow_response));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let config = Config {
            url: Url::new(format!("http://{address}/slow")).unwrap(),
            selector: Selector::new("body".to_owned()).unwrap(),
            mode: Mode::Simple,
            wait_seconds: None,
            exclude_selectors: Vec::new(),
            normalize_whitespace: false,
            poll_interval_minutes: None,
        };
        let error = poll_with_timeout(
            &reqwest::Client::new(),
            config,
            std::time::Duration::from_millis(50),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Request(error) if error.is_timeout()));
        server.abort();
    }

    #[tokio::test]
    async fn rejects_http_response_bodies_over_the_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/large", get(large_response));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let response = reqwest::get(format!("http://{address}/large"))
            .await
            .unwrap();
        let error = response_text(response, 32).await.unwrap_err();

        assert!(matches!(
            error,
            Error::ResponseBodyTooLarge { max_bytes: 32 }
        ));
        server.abort();
    }

    #[tokio::test]
    async fn accepts_http_response_body_at_the_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/large", get(large_response));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let response = reqwest::get(format!("http://{address}/large"))
            .await
            .unwrap();
        let expected = "this response body is deliberately larger than the configured test limit";
        let body = response_text(response, expected.len()).await.unwrap();

        assert_eq!(body, expected);
        server.abort();
    }

    #[tokio::test]
    async fn enforces_body_limit_when_content_length_is_missing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/chunked", get(chunked_large_response));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let response = reqwest::get(format!("http://{address}/chunked"))
            .await
            .unwrap();
        assert_eq!(response.content_length(), None);
        let error = response_text(response, 32).await.unwrap_err();

        assert!(matches!(
            error,
            Error::ResponseBodyTooLarge { max_bytes: 32 }
        ));
        server.abort();
    }
}
