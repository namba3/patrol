use clap::Parser;
use env_logger::Env;
use futures::SinkExt;
use log::{error, info};

use patrol::application::app::{DocChangeBatch, DocStatus, DocUpdateInfo};
use patrol::application::{App, SelectivePoller};
use patrol::infrastructure::{
    ChangeHistoryEntry, HttpPoller, PlaywrightPoller, TomlChangeHistoryRepository,
    TomlConfigRepository, TomlDataRepository, HISTORY_LIMIT, MAX_HISTORY_LIMIT,
};

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
use futures::stream::StreamExt;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

const DOC_UPDATE_CHANNEL_CAPACITY: usize = 128;
const HISTORY_CHANNEL_CAPACITY: usize = 32;
const HISTORY_WRITE_ATTEMPTS: usize = 3;
const HISTORY_PAGE_SIZE: usize = 50;
const MAX_HISTORY_PAGE_SIZE: usize = 100;

fn should_stop_for_input(line: &std::io::Result<Option<String>>) -> bool {
    match line {
        Ok(Some(line)) => line == "q",
        Ok(None) | Err(_) => true,
    }
}

#[derive(Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(
        short,
        long,
        help = "Specify the config file.",
        default_value = "./config.toml"
    )]
    config_path: String,
    #[clap(
        short,
        long,
        help = "Specify the data file.",
        default_value = "./data.toml"
    )]
    data_path: String,
    #[clap(
        long,
        help = "Specify the change history file.",
        default_value = "./history.toml"
    )]
    history_path: String,
    #[clap(
        long,
        help = "Specify how many recent change history entries to keep (1-1000).",
        default_value_t = HISTORY_LIMIT,
        value_parser = parse_history_limit
    )]
    history_limit: usize,
    #[clap(
        short('p'),
        long,
        help = "Specify the worker num for full mode.",
        default_value_t = 10
    )]
    worker_num: u8,
    #[clap(
        long,
        help = "Specify the maximum concurrent requests for simple mode.",
        default_value_t = 10
    )]
    simple_worker_num: u16,
    #[clap(
        short('i'),
        long,
        help = "Specify the patrol interval in minutes.",
        default_value_t = 1
    )]
    interval_minutes: u16,
    #[clap(long, help = "Patrol just once.")]
    once: bool,
}

fn parse_history_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|error| format!("invalid history limit: {error}"))?;
    if (1..=MAX_HISTORY_LIMIT).contains(&limit) {
        Ok(limit)
    } else {
        Err(format!(
            "history limit must be between 1 and {MAX_HISTORY_LIMIT}"
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    env_logger::Builder::from_env(Env::default().default_filter_or("patrol=info")).init();

    info!("config_path:      {}", args.config_path);
    info!("data_path:        {}", args.data_path);
    info!("history_path:     {}", args.history_path);
    info!("history_limit:    {}", args.history_limit);
    info!("interval_minutes: {}", args.interval_minutes);
    info!("worker_num:  {:?}", args.worker_num);
    info!("simple_worker_num: {}", args.simple_worker_num);

    let history_repo =
        TomlChangeHistoryRepository::with_limit(&args.history_path, args.history_limit).await?;
    let (tx_history, rx_history) = mpsc::channel::<DocChangeBatch>(HISTORY_CHANNEL_CAPACITY);
    let (tx_history_snapshot, history) = watch::channel(history_repo.entries().to_vec());
    let history_writer = tokio::spawn(run_history_writer(
        rx_history,
        history_repo,
        tx_history_snapshot,
    ));

    let (tx_doc_update, mut rx_doc_update) =
        tokio::sync::mpsc::channel::<DocUpdateInfo>(DOC_UPDATE_CHANNEL_CAPACITY);
    let (tx, rx) = broadcast::channel(100);
    let (tx_status, status) = watch::channel(Vec::new());
    let web_app_state = Arc::new(AppState {
        rx,
        status,
        history,
    });
    let web_app = build_web_app(web_app_state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    let web_app = tokio::spawn(async { axum::serve(listener, web_app).await });
    info!("websocket server is listening on ws://0.0.0.0:3000/");

    let config_repo = TomlConfigRepository::new(&args.config_path).await?;
    let data_repo = TomlDataRepository::new(&args.data_path).await?;

    let full_mode_poller = PlaywrightPoller::new(args.worker_num).await?;
    let simple_mode_poller = HttpPoller::new(args.simple_worker_num as usize);

    let poller = SelectivePoller::new(full_mode_poller, simple_mode_poller);

    let interval_period_secs = args.interval_minutes.max(1) as u64 * 60;
    let interval_limit = if args.once { Some(1) } else { None };

    info!("start app.");
    let patrol_app = App::new(
        config_repo,
        data_repo,
        poller,
        interval_period_secs,
        interval_limit,
    );

    let _message_dealer = tokio::spawn(async move {
        while let Some(x) = rx_doc_update.recv().await {
            let msg = serde_json::to_string(&x).unwrap();

            if let Err(why) = tx.send(msg) {
                log::warn!("{why}");
            }
        }
    });

    let (tx_command, rx_command) = oneshot::channel();
    tokio::spawn(async {
        let stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();

        loop {
            let line = lines.next_line().await;
            if should_stop_for_input(&line) {
                break;
            }
        }

        let _ = tx_command.send(());
    });

    tokio::select! {
        _quit = rx_command => (),
        result = web_app  => {
            if let Err(why) = result {
                error!("{why}")
            }
        },
        result = patrol_app.run_with_history(tx_doc_update, tx_status, Some(tx_history)) => {
            if let Err(why) = result {
                error!("{why}")
            }
        },
    }

    history_writer.await?;

    Ok(())
}

async fn run_history_writer(
    mut rx_history: mpsc::Receiver<DocChangeBatch>,
    mut history_repo: TomlChangeHistoryRepository,
    tx_history_snapshot: watch::Sender<Vec<ChangeHistoryEntry>>,
) {
    while let Some(change_batch) = rx_history.recv().await {
        let changes = change_batch
            .changes
            .into_iter()
            .map(|entry| {
                (
                    entry.id,
                    entry.timestamp_unix_ms,
                    entry.content,
                    entry.content_truncated,
                )
            })
            .collect::<Vec<_>>();
        let mut saved = false;
        for attempt in 1..=HISTORY_WRITE_ATTEMPTS {
            match history_repo.record_batch(changes.clone()).await {
                Ok(()) => {
                    tx_history_snapshot.send_replace(history_repo.entries().to_vec());
                    saved = true;
                    break;
                }
                Err(error) if attempt < HISTORY_WRITE_ATTEMPTS => {
                    log::warn!("failed to save change history (attempt {attempt}): {error}");
                    tokio::time::sleep(std::time::Duration::from_millis(100 * attempt as u64))
                        .await;
                }
                Err(error) => {
                    log::error!("failed to save change history after {attempt} attempts: {error}")
                }
            }
        }
        let _ = change_batch.persisted.send(saved);
    }
}

#[derive(Debug)]
struct AppState {
    rx: broadcast::Receiver<String>,
    status: watch::Receiver<Vec<DocStatus>>,
    history: watch::Receiver<Vec<ChangeHistoryEntry>>,
}

async fn web_ui() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

fn build_web_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(websocket_handler))
        .route("/ui", get(web_ui))
        .route("/api/v1/status", get(status_handler))
        .route("/api/v1/history", get(history_handler))
        .layer(Extension(state))
}

async fn status_handler(Extension(state): Extension<Arc<AppState>>) -> Json<Vec<DocStatus>> {
    Json(state.status.borrow().clone())
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
    Extension(state): Extension<Arc<AppState>>,
) -> Json<ChangeHistoryPage> {
    let limit = filter
        .limit
        .unwrap_or(HISTORY_PAGE_SIZE)
        .clamp(1, MAX_HISTORY_PAGE_SIZE);
    let offset = filter.offset.unwrap_or_default();
    let entries = state.history.borrow();
    let total = entries
        .iter()
        .filter(|entry| filter.id.as_deref().is_none_or(|id| id == entry.id))
        .count();
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
    Extension(state): Extension<Arc<AppState>>,
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

async fn websocket(stream: WebSocket, state: Arc<AppState>, filter: WebSocketFilter) {
    let (mut sender, _receiver) = stream.split();

    let mut rx = state.rx.resubscribe();

    while let Some(msg) = next_broadcast_message(&mut rx).await {
        if !filter.matches(&msg) {
            continue;
        }
        if let Err(why) = sender.send(msg.into()).await {
            log::warn!("failed to send a WebSocket message: {why}");
            break;
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
    use patrol::application::app::{
        DocChangeBatch, DocChangeContent, DocStatus, DocUpdateEvent, DocUpdateInfo,
    };
    use patrol::infrastructure::{ChangeHistoryEntry, TomlChangeHistoryRepository};

    use super::{
        build_web_app, history_handler, next_broadcast_message, parse_history_limit,
        run_history_writer, should_stop_for_input, status_handler, web_ui, AppState, HistoryFilter,
        WebSocketFilter, MAX_HISTORY_PAGE_SIZE,
    };

    #[test]
    fn stdin_shutdown_recognizes_quit_eof_and_errors() {
        assert!(should_stop_for_input(&Ok(Some("q".to_owned()))));
        assert!(!should_stop_for_input(&Ok(Some("continue".to_owned()))));
        assert!(should_stop_for_input(&Ok(None)));
        assert!(should_stop_for_input(&Err(std::io::Error::other(
            "stdin failed",
        ))));
    }

    #[test]
    fn websocket_failure_message_includes_event_and_failure_details() {
        let message = DocUpdateInfo {
            event: DocUpdateEvent::PollFailed,
            id: "Page".to_owned(),
            url: "https://example.com/page".to_owned(),
            timestamp: "2026-10-03 12:00:00".to_owned(),
            consecutive_failures: Some(2),
            error: Some("request failed".to_owned()),
        };

        let json = serde_json::to_value(message).unwrap();

        assert_eq!(json["event"], "poll_failed");
        assert_eq!(json["id"], "Page");
        assert_eq!(json["consecutive_failures"], 2);
        assert_eq!(json["error"], "request failed");
    }

    #[test]
    fn history_limit_parser_accepts_supported_values_only() {
        assert_eq!(parse_history_limit("1").unwrap(), 1);
        assert_eq!(parse_history_limit("1000").unwrap(), 1000);
        assert!(parse_history_limit("0").is_err());
        assert!(parse_history_limit("1001").is_err());
        assert!(parse_history_limit("many").is_err());
    }

    #[tokio::test]
    async fn websocket_receiver_continues_after_lag_and_stops_when_closed() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(1);
        tx.send("older event".to_owned()).unwrap();
        tx.send("latest event".to_owned()).unwrap();

        assert_eq!(
            next_broadcast_message(&mut rx).await.as_deref(),
            Some("latest event")
        );

        drop(tx);
        assert_eq!(next_broadcast_message(&mut rx).await, None);
    }

    #[tokio::test]
    async fn status_endpoint_returns_the_latest_snapshot() {
        let (_events_tx, events_rx) = tokio::sync::broadcast::channel(1);
        let (status_tx, status_rx) = tokio::sync::watch::channel(vec![DocStatus {
            id: "Page".to_owned(),
            url: "https://example.com/page".to_owned(),
            status: "failed".to_owned(),
            last_updated_unix_ms: None,
            last_checked_unix_ms: None,
            last_attempted_unix_ms: Some(1_791_027_296_000),
            last_success_unix_ms: None,
            consecutive_failures: 2,
            last_error: Some("request failed".to_owned()),
        }]);
        drop(status_tx);
        let state = std::sync::Arc::new(AppState {
            rx: events_rx,
            status: status_rx,
            history: tokio::sync::watch::channel(Vec::new()).1,
        });

        let axum::Json(statuses) = status_handler(axum::Extension(state)).await;

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].id, "Page");
        assert_eq!(statuses[0].status, "failed");
        assert_eq!(statuses[0].consecutive_failures, 2);
        assert_eq!(statuses[0].last_error.as_deref(), Some("request failed"));
    }

    #[tokio::test]
    async fn history_endpoint_filters_by_id_and_returns_newest_first() {
        let (_events_tx, events_rx) = tokio::sync::broadcast::channel(1);
        let (_status_tx, status_rx) = tokio::sync::watch::channel(Vec::new());
        let entries = vec![
            ChangeHistoryEntry {
                id: "a".into(),
                timestamp_unix_ms: 1,
                previous_content: None,
                previous_truncated: false,
                content: "old".into(),
                content_truncated: false,
            },
            ChangeHistoryEntry {
                id: "b".into(),
                timestamp_unix_ms: 2,
                previous_content: None,
                previous_truncated: false,
                content: "other".into(),
                content_truncated: false,
            },
            ChangeHistoryEntry {
                id: "a".into(),
                timestamp_unix_ms: 3,
                previous_content: Some("old".into()),
                previous_truncated: false,
                content: "new".into(),
                content_truncated: false,
            },
        ];
        let (_history_tx, history_rx) = tokio::sync::watch::channel(entries);
        let state = std::sync::Arc::new(AppState {
            rx: events_rx,
            status: status_rx,
            history: history_rx,
        });

        let axum::Json(entries) = history_handler(
            axum::extract::Query(HistoryFilter {
                id: Some("a".into()),
                ..HistoryFilter::default()
            }),
            axum::Extension(state.clone()),
        )
        .await;

        assert_eq!(
            entries
                .entries
                .iter()
                .map(|entry| entry.timestamp_unix_ms)
                .collect::<Vec<_>>(),
            vec![3, 1]
        );
        assert_eq!(entries.total, 2);
        assert_eq!(entries.offset, 0);
        assert!(!entries.has_older);
        assert!(!entries.has_newer);
        let payload = serde_json::to_value(&entries).unwrap();
        assert_eq!(payload["entries"][0]["timestamp_unix_ms"], 3);
        assert_eq!(payload["offset"], 0);
        assert_eq!(payload["limit"], 50);
        assert_eq!(payload["total"], 2);
        assert!(!payload["has_older"].as_bool().unwrap());
        assert!(!payload["has_newer"].as_bool().unwrap());

        let axum::Json(older_page) = history_handler(
            axum::extract::Query(HistoryFilter {
                id: Some("a".into()),
                limit: Some(1),
                offset: Some(1),
            }),
            axum::Extension(state.clone()),
        )
        .await;
        assert_eq!(older_page.entries.len(), 1);
        assert_eq!(older_page.entries[0].timestamp_unix_ms, 1);
        assert!(older_page.has_newer);
        assert!(!older_page.has_older);

        let axum::Json(minimum_page) = history_handler(
            axum::extract::Query(HistoryFilter {
                id: Some("a".into()),
                limit: Some(0),
                offset: Some(0),
            }),
            axum::Extension(state.clone()),
        )
        .await;
        assert_eq!(minimum_page.limit, 1);
        assert_eq!(minimum_page.entries.len(), 1);
        assert_eq!(minimum_page.entries[0].timestamp_unix_ms, 3);
        assert!(minimum_page.has_older);

        let axum::Json(maximum_page) = history_handler(
            axum::extract::Query(HistoryFilter {
                id: Some("a".into()),
                limit: Some(usize::MAX),
                offset: Some(0),
            }),
            axum::Extension(state.clone()),
        )
        .await;
        assert_eq!(maximum_page.limit, MAX_HISTORY_PAGE_SIZE);
        assert_eq!(maximum_page.entries.len(), 2);

        let axum::Json(empty_page) = history_handler(
            axum::extract::Query(HistoryFilter {
                id: Some("missing".into()),
                ..HistoryFilter::default()
            }),
            axum::Extension(state),
        )
        .await;
        assert!(empty_page.entries.is_empty());
        assert_eq!(empty_page.total, 0);
        assert!(!empty_page.has_older);
        assert!(!empty_page.has_newer);
    }

    #[tokio::test]
    async fn history_route_returns_a_filtered_page_over_http() {
        let (_events_tx, events_rx) = tokio::sync::broadcast::channel(1);
        let (_status_tx, status_rx) = tokio::sync::watch::channel(Vec::new());
        let entries = [("a", 1), ("b", 2), ("a", 3)]
            .into_iter()
            .map(|(id, timestamp_unix_ms)| ChangeHistoryEntry {
                id: id.to_owned(),
                timestamp_unix_ms,
                previous_content: None,
                previous_truncated: false,
                content: format!("content-{timestamp_unix_ms}"),
                content_truncated: false,
            })
            .collect();
        let state = std::sync::Arc::new(AppState {
            rx: events_rx,
            status: status_rx,
            history: tokio::sync::watch::channel(entries).1,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, build_web_app(state)).await.unwrap();
        });

        let response = reqwest::get(format!(
            "http://{address}/api/v1/history?id=a&limit=1&offset=0"
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body = response.text().await.unwrap();
        let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(payload["entries"][0]["timestamp_unix_ms"], 3);
        assert_eq!(payload["offset"], 0);
        assert_eq!(payload["limit"], 1);
        assert_eq!(payload["total"], 2);
        assert!(payload["has_older"].as_bool().unwrap());
        assert!(!payload["has_newer"].as_bool().unwrap());

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn history_writer_drains_queued_changes_before_stopping() {
        let path = std::env::temp_dir().join(format!(
            "patrol-history-writer-{}.toml",
            uuid::Uuid::new_v4()
        ));
        let path_string = path.to_string_lossy().into_owned();
        let repository = TomlChangeHistoryRepository::new(&path_string)
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (snapshot_tx, _snapshot_rx) = tokio::sync::watch::channel(Vec::new());
        let writer = tokio::spawn(run_history_writer(rx, repository, snapshot_tx));
        let (persisted_tx, persisted_rx) = tokio::sync::oneshot::channel();
        tx.send(DocChangeBatch {
            changes: vec![
                DocChangeContent {
                    id: "page".into(),
                    timestamp_unix_ms: 1,
                    content: "first".into(),
                    content_truncated: false,
                },
                DocChangeContent {
                    id: "page".into(),
                    timestamp_unix_ms: 2,
                    content: "second".into(),
                    content_truncated: false,
                },
            ],
            persisted: persisted_tx,
        })
        .await
        .unwrap();
        drop(tx);
        writer.await.unwrap();
        assert!(persisted_rx.await.unwrap());

        let repository = TomlChangeHistoryRepository::new(&path_string)
            .await
            .unwrap();
        assert_eq!(repository.entries().len(), 2);
        assert_eq!(repository.entries()[1].content, "second");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn history_writer_reports_persistence_failure_after_retries() {
        let path = std::env::temp_dir().join(format!(
            "patrol-history-writer-error-{}.toml",
            uuid::Uuid::new_v4()
        ));
        let path_string = path.to_string_lossy().into_owned();
        let repository = TomlChangeHistoryRepository::new(&path_string)
            .await
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let (snapshot_tx, snapshot_rx) = tokio::sync::watch::channel(Vec::new());
        let writer = tokio::spawn(run_history_writer(rx, repository, snapshot_tx));
        let (persisted_tx, persisted_rx) = tokio::sync::oneshot::channel();
        tx.send(DocChangeBatch {
            changes: vec![DocChangeContent {
                id: "page".into(),
                timestamp_unix_ms: 1,
                content: "content".into(),
                content_truncated: false,
            }],
            persisted: persisted_tx,
        })
        .await
        .unwrap();
        drop(tx);
        writer.await.unwrap();

        assert!(!persisted_rx.await.unwrap());
        assert!(snapshot_rx.borrow().is_empty());
        std::fs::remove_dir(path).unwrap();
    }

    #[tokio::test]
    async fn bundled_web_ui_uses_the_status_api_and_websocket() {
        let axum::response::Html(page) = web_ui().await;

        assert!(page.contains("/api/v1/status"));
        assert!(page.contains("/api/v1/history"));
        assert!(page.contains("history-older"));
        assert!(page.contains("history-newer"));
        assert!(page.contains("history-retry"));
        assert!(page.contains("new WebSocket"));
        assert!(page.contains("event-type"));
    }

    #[test]
    fn websocket_filter_matches_target_and_event() {
        let message =
            r#"{"event":"poll_failed","id":"Page","url":"https://example.com","timestamp":"now"}"#;
        let filter = WebSocketFilter {
            id: Some("Page".to_owned()),
            event: Some("poll_failed".to_owned()),
        };

        assert!(filter.has_valid_event());
        assert!(filter.matches(message));
        assert!(!WebSocketFilter {
            id: Some("OtherPage".to_owned()),
            event: Some("poll_failed".to_owned()),
        }
        .matches(message));
        assert!(!WebSocketFilter {
            id: None,
            event: Some("changed".to_owned()),
        }
        .matches(message));
        assert!(!WebSocketFilter {
            id: None,
            event: Some("unknown".to_owned()),
        }
        .has_valid_event());
        assert!(WebSocketFilter::default().matches(message));
    }
}
