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
use tokio::sync::{broadcast, watch};

use crate::{application::app::DocStatus, infrastructure::ChangeHistoryEntry};

const HISTORY_PAGE_SIZE: usize = 50;
const MAX_HISTORY_PAGE_SIZE: usize = 100;

#[derive(Debug)]
pub struct WebAppState {
    rx: broadcast::Receiver<String>,
    status: watch::Receiver<Vec<DocStatus>>,
    history: watch::Receiver<Vec<ChangeHistoryEntry>>,
}

impl WebAppState {
    pub fn new(
        rx: broadcast::Receiver<String>,
        status: watch::Receiver<Vec<DocStatus>>,
        history: watch::Receiver<Vec<ChangeHistoryEntry>>,
    ) -> Self {
        Self {
            rx,
            status,
            history,
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
    match serde_json::to_vec(&*status) {
        Ok(body) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(error) => {
            log::error!("failed to serialize status snapshot: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct HistoryFilter {
    id: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, serde::Serialize)]
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
) -> Json<ChangeHistoryPage> {
    let limit = filter
        .limit
        .unwrap_or(HISTORY_PAGE_SIZE)
        .clamp(1, MAX_HISTORY_PAGE_SIZE);
    let requested_offset = filter.offset.unwrap_or_default();
    let entries = state.history.borrow();
    let total = entries
        .iter()
        .filter(|entry| filter.id.as_deref().is_none_or(|id| id == entry.id))
        .count();
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
    Json(ChangeHistoryPage {
        has_older: offset.saturating_add(page_entries.len()) < total,
        has_newer: offset > 0,
        entries: page_entries,
        offset,
        limit,
        total,
    })
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

    loop {
        match next_websocket_input(&mut receiver, &mut rx).await {
            WebSocketInput::Broadcast(message) => {
                if !filter.matches(&message) {
                    continue;
                }
                if let Err(why) = sender.send(message.into()).await {
                    log::warn!("failed to send a WebSocket message: {why}");
                    break;
                }
            }
            WebSocketInput::ClientMessage => (),
            WebSocketInput::Closed => break,
        }
    }
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
        history_handler, next_broadcast_message, next_websocket_input, status_handler, web_ui,
        HistoryFilter, WebAppState, WebSocketFilter, WebSocketInput,
    };
    use crate::{application::app::DocStatus, infrastructure::ChangeHistoryEntry};
    use axum::{extract::Query, Extension};
    use std::sync::Arc;
    use tokio::sync::{broadcast, watch};

    fn state_with_history(entries: Vec<ChangeHistoryEntry>) -> Arc<WebAppState> {
        let (_events_tx, events_rx) = broadcast::channel(1);
        let (_status_tx, status_rx) = watch::channel(Vec::<DocStatus>::new());
        let (_history_tx, history_rx) = watch::channel(entries);
        Arc::new(WebAppState::new(events_rx, status_rx, history_rx))
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

        let axum::Json(page) = history_handler(
            Query(HistoryFilter {
                id: Some("page".into()),
                limit: Some(1),
                offset: Some(1),
            }),
            Extension(state),
        )
        .await;

        assert_eq!(page.total, 2);
        assert_eq!(page.offset, 1);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].timestamp_unix_ms, 1);
        assert!(page.has_newer);
        assert!(!page.has_older);
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
        ));

        let response = status_handler(Extension(state)).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload[0]["id"], "page");
        assert_eq!(payload[0]["consecutive_failures"], 2);
    }
}
