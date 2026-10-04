use std::sync::Arc;

use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        Extension, Query,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, watch};

use crate::{
    application::app::{DocStatus, DocUpdateInfo},
    infrastructure::ChangeHistoryEntry,
};

const HISTORY_PAGE_SIZE: usize = 50;
const MAX_HISTORY_PAGE_SIZE: usize = 100;

#[derive(Debug)]
pub struct WebAppState {
    rx: broadcast::Receiver<String>,
    status: watch::Receiver<Vec<DocStatus>>,
    history: watch::Receiver<Vec<ChangeHistoryEntry>>,
    shutdown: watch::Receiver<bool>,
}

impl WebAppState {
    pub fn new(
        rx: broadcast::Receiver<String>,
        status: watch::Receiver<Vec<DocStatus>>,
        history: watch::Receiver<Vec<ChangeHistoryEntry>>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        Self {
            rx,
            status,
            history,
            shutdown,
        }
    }
}

pub async fn run_notification_relay(
    mut updates: mpsc::Receiver<DocUpdateInfo>,
    events: broadcast::Sender<String>,
) {
    while let Some(update) = updates.recv().await {
        let message = match serde_json::to_string(&update) {
            Ok(message) => message,
            Err(error) => {
                log::error!("failed to serialize a document update: {error}");
                continue;
            }
        };

        if let Err(error) = events.send(message) {
            log::warn!("{error}");
        }
    }
}

async fn web_ui() -> Html<&'static str> {
    Html(include_str!("../../web/index.html"))
}

async fn health_handler() -> &'static str {
    "ok"
}

pub fn build_web_app(state: Arc<WebAppState>) -> Router {
    Router::new()
        .route("/", get(websocket_handler))
        .route("/ui", get(web_ui))
        .route("/healthz", get(health_handler))
        .route("/api/v1/status", get(status_handler))
        .route("/api/v1/history", get(history_handler))
        .layer(Extension(state))
}

async fn status_handler(Extension(state): Extension<Arc<WebAppState>>) -> Response {
    let status = state.status.borrow();
    let mut response = match serde_json::to_vec(&*status) {
        Ok(body) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(error) => {
            log::error!("failed to serialize status snapshot: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    };
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Debug, Default, serde::Deserialize)]
struct HistoryFilter {
    id: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct ChangeHistoryPage {
    entries: Vec<ChangeHistoryEntry>,
    offset: usize,
    limit: usize,
    total: usize,
    has_older: bool,
    has_newer: bool,
}

async fn history_handler(
    Query(filter): Query<HistoryFilter>,
    Extension(state): Extension<Arc<WebAppState>>,
) -> Response {
    let limit = filter
        .limit
        .unwrap_or(HISTORY_PAGE_SIZE)
        .clamp(1, MAX_HISTORY_PAGE_SIZE);
    let requested_offset = filter.offset.unwrap_or_default();
    let entries = state.history.borrow();
    let total = match filter.id.as_deref() {
        Some(id) => entries.iter().filter(|entry| id == entry.id).count(),
        None => entries.len(),
    };
    let offset = if requested_offset >= total {
        total.saturating_sub(1) / limit * limit
    } else {
        requested_offset
    };
    let page_entries = entries
        .iter()
        .rev()
        .filter(|entry| filter.id.as_deref().is_none_or(|id| id == entry.id))
        .skip(offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(ChangeHistoryPage {
            has_older: offset.saturating_add(page_entries.len()) < total,
            has_newer: offset > 0,
            entries: page_entries,
            offset,
            limit,
            total,
        }),
    )
        .into_response()
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Query(filter): Query<WebSocketFilter>,
    Extension(state): Extension<Arc<WebAppState>>,
) -> Response {
    if !filter.has_valid_event() {
        return (
            StatusCode::BAD_REQUEST,
            "event must be changed, poll_failed, or poll_recovered",
        )
            .into_response();
    }

    ws.on_upgrade(|socket| websocket(socket, state, filter))
        .into_response()
}

#[derive(Debug, Default, serde::Deserialize)]
struct WebSocketFilter {
    id: Option<String>,
    event: Option<String>,
}

impl WebSocketFilter {
    fn has_valid_event(&self) -> bool {
        self.event
            .as_deref()
            .is_none_or(|event| matches!(event, "changed" | "poll_failed" | "poll_recovered"))
    }

    fn matches(&self, message: &str) -> bool {
        if self.id.is_none() && self.event.is_none() {
            return true;
        }

        let Ok(message) = serde_json::from_str::<NotificationFilterFields>(message) else {
            return false;
        };
        self.id.as_deref().is_none_or(|id| id == message.id)
            && self
                .event
                .as_deref()
                .is_none_or(|event| event == message.event)
    }
}

#[derive(serde::Deserialize)]
struct NotificationFilterFields {
    id: String,
    event: String,
}

async fn websocket(stream: WebSocket, state: Arc<WebAppState>, filter: WebSocketFilter) {
    let (mut sender, mut receiver) = stream.split();

    let mut rx = state.rx.resubscribe();
    let mut shutdown = state.shutdown.clone();

    if *shutdown.borrow() {
        send_websocket_close(&mut sender).await;
        return;
    }

    loop {
        let input = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    send_websocket_close(&mut sender).await;
                    break;
                }
                continue;
            }
            input = next_websocket_input(&mut receiver, &mut rx) => input,
        };
        match input {
            WebSocketInput::Broadcast(message) => {
                if !filter.matches(&message) {
                    continue;
                }
                match send_websocket_message(&mut sender, &mut shutdown, message.into()).await {
                    WebSocketSend::Sent(Ok(())) => (),
                    WebSocketSend::Sent(Err(why)) => {
                        log::warn!("failed to send a WebSocket message: {why}");
                        break;
                    }
                    WebSocketSend::Shutdown => {
                        send_websocket_close(&mut sender).await;
                        break;
                    }
                }
            }
            WebSocketInput::ClientMessage => (),
            WebSocketInput::Closed => break,
        }
    }
}

enum WebSocketSend<E> {
    Sent(Result<(), E>),
    Shutdown,
}

async fn send_websocket_message<S, E>(
    sender: &mut S,
    shutdown: &mut watch::Receiver<bool>,
    message: axum::extract::ws::Message,
) -> WebSocketSend<E>
where
    S: futures::Sink<axum::extract::ws::Message, Error = E> + Unpin,
{
    if *shutdown.borrow() {
        return WebSocketSend::Shutdown;
    }

    tokio::select! {
        biased;
        changed = shutdown.changed() => {
            let _ = changed;
            WebSocketSend::Shutdown
        }
        result = sender.send(message) => WebSocketSend::Sent(result),
    }
}

async fn send_websocket_close<S, E>(sender: &mut S)
where
    S: futures::Sink<axum::extract::ws::Message, Error = E> + Unpin,
{
    let close = sender.send(axum::extract::ws::Message::Close(None));
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), close).await;
}

enum WebSocketInput {
    Broadcast(String),
    ClientMessage,
    Closed,
}

async fn next_websocket_input<S, E>(
    receiver: &mut S,
    rx: &mut broadcast::Receiver<String>,
) -> WebSocketInput
where
    S: futures::Stream<Item = Result<axum::extract::ws::Message, E>> + Unpin,
    E: std::fmt::Display,
{
    tokio::select! {
        message = next_broadcast_message(rx) => match message {
            Some(message) => WebSocketInput::Broadcast(message),
            None => WebSocketInput::Closed,
        },
        incoming = receiver.next() => match incoming {
            Some(Ok(axum::extract::ws::Message::Close(_))) | None => WebSocketInput::Closed,
            Some(Ok(_)) => WebSocketInput::ClientMessage,
            Some(Err(error)) => {
                log::warn!("failed to receive a WebSocket message: {error}");
                WebSocketInput::Closed
            }
        }
    }
}

async fn next_broadcast_message(rx: &mut broadcast::Receiver<String>) -> Option<String> {
    loop {
        match rx.recv().await {
            Ok(message) => return Some(message),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("WebSocket client lagged; skipped {skipped} buffered events");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        history_handler, next_broadcast_message, next_websocket_input, run_notification_relay,
        send_websocket_message, status_handler, web_ui, ChangeHistoryPage, HistoryFilter,
        WebAppState, WebSocketFilter, WebSocketInput, WebSocketSend,
    };
    use crate::{
        application::app::{DocStatus, DocUpdateEvent, DocUpdateInfo},
        infrastructure::ChangeHistoryEntry,
    };
    use axum::{extract::Query, response::Response, Extension};
    use std::sync::Arc;
    use tokio::sync::{broadcast, mpsc, watch};

    struct PendingWebSocketSink;

    impl futures::Sink<axum::extract::ws::Message> for PendingWebSocketSink {
        type Error = std::convert::Infallible;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn start_send(
            self: std::pin::Pin<&mut Self>,
            _item: axum::extract::ws::Message,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }
    }

    fn state_with_history(entries: Vec<ChangeHistoryEntry>) -> Arc<WebAppState> {
        let (_events_tx, events_rx) = broadcast::channel(1);
        let (_status_tx, status_rx) = watch::channel(Vec::<DocStatus>::new());
        let (_history_tx, history_rx) = watch::channel(entries);
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        Arc::new(WebAppState::new(
            events_rx,
            status_rx,
            history_rx,
            shutdown_rx,
        ))
    }

    async fn history_page(response: Response) -> ChangeHistoryPage {
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn history_page_filters_and_orders_entries() {
        let state = state_with_history(vec![
            ChangeHistoryEntry {
                id: "page".into(),
                timestamp_unix_ms: 1,
                previous_content: None,
                previous_truncated: false,
                content: "old".into(),
                content_truncated: false,
            },
            ChangeHistoryEntry {
                id: "other".into(),
                timestamp_unix_ms: 2,
                previous_content: None,
                previous_truncated: false,
                content: "other".into(),
                content_truncated: false,
            },
            ChangeHistoryEntry {
                id: "page".into(),
                timestamp_unix_ms: 3,
                previous_content: Some("old".into()),
                previous_truncated: false,
                content: "new".into(),
                content_truncated: false,
            },
        ]);

        let page = history_page(
            history_handler(
                Query(HistoryFilter {
                    id: Some("page".into()),
                    limit: Some(1),
                    offset: Some(1),
                }),
                Extension(state.clone()),
            )
            .await,
        )
        .await;

        assert_eq!(page.total, 2);
        assert_eq!(page.offset, 1);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].timestamp_unix_ms, 1);
        assert!(page.has_newer);
        assert!(!page.has_older);

        let minimum_page = history_page(
            history_handler(
                Query(HistoryFilter {
                    id: Some("page".into()),
                    limit: Some(0),
                    offset: Some(0),
                }),
                Extension(state.clone()),
            )
            .await,
        )
        .await;
        assert_eq!(minimum_page.limit, 1);
        assert_eq!(minimum_page.entries[0].timestamp_unix_ms, 3);
        assert!(minimum_page.has_older);

        let maximum_page = history_page(
            history_handler(
                Query(HistoryFilter {
                    id: Some("page".into()),
                    limit: Some(usize::MAX),
                    offset: Some(0),
                }),
                Extension(state.clone()),
            )
            .await,
        )
        .await;
        assert_eq!(maximum_page.limit, 100);
        assert_eq!(maximum_page.entries.len(), 2);

        let out_of_range_page = history_page(
            history_handler(
                Query(HistoryFilter {
                    id: Some("page".into()),
                    limit: Some(1),
                    offset: Some(usize::MAX),
                }),
                Extension(state.clone()),
            )
            .await,
        )
        .await;
        assert_eq!(out_of_range_page.offset, 1);
        assert_eq!(out_of_range_page.entries[0].timestamp_unix_ms, 1);
        assert!(out_of_range_page.has_newer);
        assert!(!out_of_range_page.has_older);

        let unaligned_end_page = history_page(
            history_handler(
                Query(HistoryFilter {
                    limit: Some(2),
                    offset: Some(usize::MAX),
                    ..HistoryFilter::default()
                }),
                Extension(state.clone()),
            )
            .await,
        )
        .await;
        assert_eq!(unaligned_end_page.total, 3);
        assert_eq!(unaligned_end_page.offset, 2);
        assert_eq!(unaligned_end_page.entries.len(), 1);
        assert_eq!(unaligned_end_page.entries[0].timestamp_unix_ms, 1);
        assert!(unaligned_end_page.has_newer);
        assert!(!unaligned_end_page.has_older);

        let empty_page = history_page(
            history_handler(
                Query(HistoryFilter {
                    id: Some("missing".into()),
                    offset: Some(usize::MAX),
                    ..HistoryFilter::default()
                }),
                Extension(state),
            )
            .await,
        )
        .await;
        assert!(empty_page.entries.is_empty());
        assert_eq!(empty_page.offset, 0);
        assert_eq!(empty_page.total, 0);
        assert!(!empty_page.has_older);
        assert!(!empty_page.has_newer);
    }

    #[test]
    fn websocket_filter_validates_and_matches_event_fields() {
        let message = r#"{"event":"poll_failed","id":"page"}"#;
        let filter = WebSocketFilter {
            id: Some("page".into()),
            event: Some("poll_failed".into()),
        };

        assert!(filter.has_valid_event());
        assert!(filter.matches(message));
        assert!(!WebSocketFilter {
            id: Some("other".into()),
            event: Some("poll_failed".into()),
        }
        .matches(message));
        assert!(!WebSocketFilter {
            id: None,
            event: Some("unknown".into()),
        }
        .has_valid_event());
    }

    #[tokio::test]
    async fn bundled_ui_contains_the_status_and_history_clients() {
        let axum::response::Html(page) = web_ui().await;

        assert!(page.contains("/api/v1/status"));
        assert!(page.contains("/api/v1/history"));
        assert!(page.contains("new WebSocket"));
    }

    #[tokio::test]
    async fn broadcast_receiver_skips_lagged_events_and_stops_when_closed() {
        let (tx, mut rx) = broadcast::channel(1);
        tx.send("older".to_owned()).unwrap();
        tx.send("latest".to_owned()).unwrap();

        assert_eq!(
            next_broadcast_message(&mut rx).await.as_deref(),
            Some("latest")
        );

        drop(tx);
        assert_eq!(next_broadcast_message(&mut rx).await, None);
    }

    #[tokio::test]
    async fn websocket_input_ignores_client_data_and_stops_on_close() {
        let (_events_tx, mut events_rx) = broadcast::channel(1);
        let mut receiver = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(axum::extract::ws::Message::Text("ignored".into())),
            Ok(axum::extract::ws::Message::Close(None)),
        ]);

        assert!(matches!(
            next_websocket_input(&mut receiver, &mut events_rx).await,
            WebSocketInput::ClientMessage
        ));
        assert!(matches!(
            next_websocket_input(&mut receiver, &mut events_rx).await,
            WebSocketInput::Closed
        ));
    }

    #[tokio::test]
    async fn websocket_shutdown_interrupts_a_pending_event_send() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let mut sink = PendingWebSocketSink;
        let mut send = Box::pin(send_websocket_message(
            &mut sink,
            &mut shutdown_rx,
            axum::extract::ws::Message::Text("update".into()),
        ));

        assert!(futures::poll!(send.as_mut()).is_pending());
        shutdown_tx.send_replace(true);
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), send)
                .await
                .expect("shutdown did not interrupt the pending send"),
            WebSocketSend::Shutdown
        ));

        let (_shutdown_tx, mut already_shutdown) = watch::channel(true);
        let mut sink = PendingWebSocketSink;
        assert!(matches!(
            send_websocket_message(
                &mut sink,
                &mut already_shutdown,
                axum::extract::ws::Message::Text("late update".into()),
            )
            .await,
            WebSocketSend::Shutdown
        ));
    }

    #[tokio::test]
    async fn status_endpoint_serializes_the_latest_snapshot() {
        let (_events_tx, events_rx) = broadcast::channel(1);
        let (_status_tx, status_rx) = watch::channel(vec![DocStatus {
            id: "page".into(),
            url: "https://example.com".into(),
            status: "failed".into(),
            last_updated_unix_ms: None,
            last_checked_unix_ms: None,
            last_attempted_unix_ms: Some(1),
            last_success_unix_ms: None,
            consecutive_failures: 2,
            last_error: Some("request failed".into()),
        }]);
        let state = Arc::new(WebAppState::new(
            events_rx,
            status_rx,
            watch::channel(Vec::new()).1,
            watch::channel(false).1,
        ));

        let response = status_handler(Extension(state)).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload[0]["id"], "page");
        assert_eq!(payload[0]["consecutive_failures"], 2);
    }

    #[tokio::test]
    async fn notification_relay_serializes_and_broadcasts_app_updates() {
        let (updates_tx, updates_rx) = mpsc::channel(1);
        let (events_tx, mut events_rx) = broadcast::channel(1);
        let relay = tokio::spawn(run_notification_relay(updates_rx, events_tx));
        updates_tx
            .send(DocUpdateInfo {
                event: DocUpdateEvent::PollFailed,
                id: "Page".to_owned(),
                url: "https://example.com/page".to_owned(),
                timestamp: "2026-10-03 12:00:00".to_owned(),
                consecutive_failures: Some(2),
                error: Some("request failed".to_owned()),
            })
            .await
            .unwrap();
        drop(updates_tx);

        let message = events_rx.recv().await.unwrap();
        let payload: serde_json::Value = serde_json::from_str(&message).unwrap();
        assert_eq!(payload["event"], "poll_failed");
        assert_eq!(payload["id"], "Page");
        assert_eq!(payload["consecutive_failures"], 2);
        assert_eq!(payload["error"], "request failed");
        relay.await.unwrap();
    }

    #[tokio::test]
    async fn axum_server_stops_accepting_after_shutdown_signal() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let (_events_tx, events_rx) = broadcast::channel(1);
        let (_status_tx, status_rx) = watch::channel(Vec::<DocStatus>::new());
        let (_history_tx, history_rx) = watch::channel(Vec::new());
        let state = Arc::new(WebAppState::new(
            events_rx,
            status_rx,
            history_rx,
            shutdown_rx.clone(),
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, super::build_web_app(state))
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.changed().await;
                })
                .await
        });

        shutdown_tx.send_replace(true);
        tokio::time::timeout(std::time::Duration::from_secs(1), server)
            .await
            .expect("server did not stop after shutdown")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_sends_a_close_frame_to_connected_websockets() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (_events_tx, events_rx) = broadcast::channel(1);
        let (_status_tx, status_rx) = watch::channel(Vec::<DocStatus>::new());
        let (_history_tx, history_rx) = watch::channel(Vec::new());
        let state = Arc::new(WebAppState::new(
            events_rx,
            status_rx,
            history_rx,
            shutdown_rx.clone(),
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, super::build_web_app(state))
                .with_graceful_shutdown(async move {
                    let mut shutdown_rx = shutdown_rx;
                    let _ = shutdown_rx.changed().await;
                })
                .await
        });

        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut socket = tokio::io::BufReader::new(socket);
        socket
            .get_mut()
            .write_all(
                format!(
                    "GET / HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response_headers = String::new();
        loop {
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            response_headers.push_str(&line);
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        assert!(response_headers.starts_with("HTTP/1.1 101"));

        shutdown_tx.send_replace(true);
        let mut close_frame = [0_u8; 2];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            socket.read_exact(&mut close_frame),
        )
        .await
        .expect("websocket did not receive a close frame")
        .unwrap();
        assert_eq!(close_frame[0] & 0x0f, 0x08);

        tokio::time::timeout(std::time::Duration::from_secs(1), server)
            .await
            .expect("server did not stop after websocket closed")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn websocket_route_delivers_only_matching_filtered_events() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let (events_tx, events_rx) = broadcast::channel(8);
        let (_status_tx, status_rx) = watch::channel(Vec::<DocStatus>::new());
        let (_history_tx, history_rx) = watch::channel(Vec::new());
        let state = Arc::new(WebAppState::new(
            events_rx,
            status_rx,
            history_rx,
            shutdown_rx.clone(),
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, super::build_web_app(state))
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.changed().await;
                })
                .await
        });

        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut socket = tokio::io::BufReader::new(socket);
        socket
            .get_mut()
            .write_all(
                format!(
                    "GET /?id=page-a&event=changed HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response_headers = String::new();
        loop {
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            response_headers.push_str(&line);
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        assert!(response_headers.starts_with("HTTP/1.1 101"));

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while events_tx.receiver_count() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("websocket handler did not subscribe to broadcasts");
        for event in [
            r#"{"event":"changed","id":"page-b"}"#,
            r#"{"event":"poll_failed","id":"page-a"}"#,
            r#"{"event":"changed","id":"page-a"}"#,
        ] {
            events_tx.send(event.to_owned()).unwrap();
        }

        let mut frame_header = [0_u8; 2];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            socket.read_exact(&mut frame_header),
        )
        .await
        .expect("filtered event was not delivered")
        .unwrap();
        assert_eq!(frame_header[0] & 0x0f, 0x01);
        assert_eq!(frame_header[1] & 0x80, 0);
        let payload_len = usize::from(frame_header[1] & 0x7f);
        assert!(payload_len < 126);
        let mut payload = vec![0; payload_len];
        socket.read_exact(&mut payload).await.unwrap();
        assert_eq!(
            String::from_utf8(payload).unwrap(),
            r#"{"event":"changed","id":"page-a"}"#
        );

        shutdown_tx.send_replace(true);
        let mut close_frame = [0_u8; 2];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            socket.read_exact(&mut close_frame),
        )
        .await
        .expect("websocket did not receive a close frame")
        .unwrap();
        assert_eq!(close_frame[0] & 0x0f, 0x08);

        tokio::time::timeout(std::time::Duration::from_secs(1), server)
            .await
            .expect("server did not stop after websocket closed")
            .unwrap()
            .unwrap();
    }
}
