use clap::Parser;
use env_logger::Env;
use futures::SinkExt;
use log::{error, info};

use patrol::application::app::{DocStatus, DocUpdateInfo};
use patrol::application::{App, SelectivePoller};
use patrol::infrastructure::{
    HttpPoller, PlaywrightPoller, TomlConfigRepository, TomlDataRepository,
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
use tokio::sync::{broadcast, oneshot, watch};

const DOC_UPDATE_CHANNEL_CAPACITY: usize = 128;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    env_logger::Builder::from_env(Env::default().default_filter_or("patrol=info")).init();

    info!("config_path:      {}", args.config_path);
    info!("data_path:        {}", args.data_path);
    info!("interval_minutes: {}", args.interval_minutes);
    info!("worker_num:  {:?}", args.worker_num);
    info!("simple_worker_num: {}", args.simple_worker_num);

    let (tx_doc_update, mut rx_doc_update) =
        tokio::sync::mpsc::channel::<DocUpdateInfo>(DOC_UPDATE_CHANNEL_CAPACITY);
    let (tx, rx) = broadcast::channel(100);
    let (tx_status, status) = watch::channel(Vec::new());
    let web_app_state = Arc::new(AppState { rx, status });
    let web_app = Router::new()
        .route("/", get(websocket_handler))
        .route("/ui", get(web_ui))
        .route("/api/v1/status", get(status_handler))
        .layer(Extension(web_app_state));
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
            match line.as_ref().map(|x| x.as_ref().map(|y| y.as_str())) {
                Ok(Some("q")) => break,
                Ok(_) => (),
                Err(_why) => {
                    //error!("{why}")
                    break;
                }
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
        result = patrol_app.run_with_status(tx_doc_update, tx_status) => {
            if let Err(why) = result {
                error!("{why}")
            }
        },
    }

    Ok(())
}

#[derive(Debug)]
struct AppState {
    rx: broadcast::Receiver<String>,
    status: watch::Receiver<Vec<DocStatus>>,
}

async fn web_ui() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn status_handler(Extension(state): Extension<Arc<AppState>>) -> Json<Vec<DocStatus>> {
    Json(state.status.borrow().clone())
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
    use patrol::application::app::{DocStatus, DocUpdateEvent, DocUpdateInfo};

    use super::{next_broadcast_message, status_handler, web_ui, AppState, WebSocketFilter};

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
        });

        let axum::Json(statuses) = status_handler(axum::Extension(state)).await;

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].id, "Page");
        assert_eq!(statuses[0].status, "failed");
        assert_eq!(statuses[0].consecutive_failures, 2);
        assert_eq!(statuses[0].last_error.as_deref(), Some("request failed"));
    }

    #[tokio::test]
    async fn bundled_web_ui_uses_the_status_api_and_websocket() {
        let axum::response::Html(page) = web_ui().await;

        assert!(page.contains("/api/v1/status"));
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
