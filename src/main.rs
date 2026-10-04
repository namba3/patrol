use clap::Parser;
use env_logger::Env;
use log::{error, info};
use std::net::SocketAddr;
use std::sync::Arc;

use patrol::application::app::{DocChangeBatch, DocUpdateInfo};
use patrol::application::{App, SelectivePoller};
use patrol::infrastructure::{
    change_history_writer::run_change_history_writer,
    web::{build_web_app, run_notification_relay, WebAppState},
    HttpPoller, PlaywrightPoller, TomlChangeHistoryRepository, TomlConfigRepository,
    TomlDataRepository, HISTORY_LIMIT, MAX_HISTORY_LIMIT,
};

use tokio::io::AsyncBufReadExt;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

const DOC_UPDATE_CHANNEL_CAPACITY: usize = 128;
const HISTORY_CHANNEL_CAPACITY: usize = 32;

fn should_stop_for_input(line: &std::io::Result<Option<String>>) -> bool {
    match line {
        Ok(Some(line)) => line == "q",
        Ok(None) | Err(_) => true,
    }
}

async fn join_shutdown_task<T>(
    task: tokio::task::JoinHandle<T>,
    task_name: &str,
) -> Option<Box<dyn std::error::Error>> {
    shutdown_task_result(task.await, task_name, false)
}

fn shutdown_task_result<T>(
    result: Result<T, tokio::task::JoinError>,
    task_name: &str,
    unexpected_completion: bool,
) -> Option<Box<dyn std::error::Error>> {
    match result {
        Ok(_) if !unexpected_completion => None,
        Ok(_) => {
            let why = std::io::Error::other(format!("{task_name} task stopped unexpectedly"));
            error!("{why}");
            Some(Box::new(why))
        }
        Err(why) => {
            error!("{task_name} task failed: {why}");
            Some(Box::new(why))
        }
    }
}

enum BackgroundTaskExit {
    NotificationRelay(Option<Box<dyn std::error::Error>>),
    HistoryWriter(Option<Box<dyn std::error::Error>>),
}

async fn wait_for_background_task_exit(
    notification_relay: &mut tokio::task::JoinHandle<()>,
    history_writer: &mut tokio::task::JoinHandle<()>,
) -> BackgroundTaskExit {
    tokio::select! {
        result = notification_relay => BackgroundTaskExit::NotificationRelay(
            shutdown_task_result(result, "notification relay", true),
        ),
        result = history_writer => BackgroundTaskExit::HistoryWriter(
            shutdown_task_result(result, "change history writer", true),
        ),
    }
}

async fn wait_for_ctrl_c() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!("failed to listen for Ctrl-C: {error}");
        std::future::pending::<()>().await;
    }
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = wait_for_ctrl_c() => (),
                _ = terminate.recv() => (),
            }
        }
        Err(error) => {
            error!("failed to listen for SIGTERM: {error}");
            wait_for_ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    wait_for_ctrl_c().await;
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
        long,
        help = "Specify the HTTP and WebSocket listen address.",
        default_value = "0.0.0.0:3000"
    )]
    web_listen: SocketAddr,
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
    let mut history_writer = tokio::spawn(run_change_history_writer(
        rx_history,
        history_repo,
        tx_history_snapshot,
    ));

    let (tx_doc_update, rx_doc_update) =
        tokio::sync::mpsc::channel::<DocUpdateInfo>(DOC_UPDATE_CHANNEL_CAPACITY);
    let (tx, rx) = broadcast::channel(100);
    let (tx_status, status) = watch::channel(Vec::new());
    let (tx_web_shutdown, mut web_shutdown) = watch::channel(false);
    let web_app_state = Arc::new(WebAppState::new(rx, status, history, web_shutdown.clone()));
    let web_app = build_web_app(web_app_state);
    let listener = tokio::net::TcpListener::bind(args.web_listen).await?;
    let web_address = listener.local_addr()?;
    let mut web_app = tokio::spawn(async move {
        axum::serve(listener, web_app)
            .with_graceful_shutdown(async move {
                let _ = web_shutdown.changed().await;
            })
            .await
    });
    info!("web server is listening at http://{web_address}/ and ws://{web_address}/");

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

    let mut message_dealer = tokio::spawn(run_notification_relay(rx_doc_update, tx));

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

    let (tx_shutdown, rx_shutdown) = watch::channel(false);
    let app_future = patrol_app.run_with_history_and_shutdown(
        tx_doc_update,
        tx_status,
        Some(tx_history),
        rx_shutdown,
    );
    tokio::pin!(app_future);
    let mut web_app_finished = false;
    let mut web_error: Option<Box<dyn std::error::Error>> = None;
    let mut notification_relay_finished = false;
    let mut notification_error = None;
    let mut history_writer_finished = false;
    let mut history_writer_error = None;
    let app_result = tokio::select! {
        biased;
        result = &mut app_future => Some(result),
        _quit = rx_command => None,
        _signal = wait_for_shutdown_signal() => {
            info!("received shutdown signal; stopping after the current patrol cycle");
            None
        },
        result = &mut web_app => {
            web_app_finished = true;
            let error: Box<dyn std::error::Error> = match result {
                Ok(Ok(())) => Box::new(std::io::Error::other("web server stopped unexpectedly")),
                Ok(Err(why)) => Box::new(why),
                Err(why) => Box::new(why),
            };
            error!("web server failed: {error}");
            web_error = Some(error);
            None
        },
        task_exit = wait_for_background_task_exit(&mut message_dealer, &mut history_writer) => {
            match task_exit {
                BackgroundTaskExit::NotificationRelay(error) => {
                    notification_relay_finished = true;
                    notification_error = error;
                }
                BackgroundTaskExit::HistoryWriter(error) => {
                    history_writer_finished = true;
                    history_writer_error = error;
                }
            }
            None
        },
    };
    let app_result = match app_result {
        Some(result) => result,
        None => {
            tx_shutdown.send_replace(true);
            app_future.await
        }
    };
    let app_error: Option<Box<dyn std::error::Error>> = if let Err(why) = app_result {
        error!("{why}");
        Some(Box::new(why))
    } else {
        None
    };

    if !notification_relay_finished {
        notification_error = join_shutdown_task(message_dealer, "notification relay").await;
    }
    if !history_writer_finished {
        history_writer_error = join_shutdown_task(history_writer, "change history writer").await;
    }

    tx_web_shutdown.send_replace(true);
    if !web_app_finished {
        match tokio::time::timeout(std::time::Duration::from_secs(5), &mut web_app).await {
            Ok(Ok(Ok(()))) => (),
            Ok(Ok(Err(why))) => return Err(why.into()),
            Ok(Err(why)) => return Err(why.into()),
            Err(_) => {
                log::warn!("web server did not stop within 5 seconds; aborting it");
                web_app.abort();
                let _ = web_app.await;
            }
        }
    }

    if let Some(why) = app_error
        .or(web_error)
        .or(notification_error)
        .or(history_writer_error)
    {
        return Err(why);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use patrol::infrastructure::{
        web::{build_web_app, WebAppState},
        ChangeHistoryEntry,
    };

    use super::{
        join_shutdown_task, parse_history_limit, should_stop_for_input, shutdown_task_result,
        wait_for_background_task_exit, BackgroundTaskExit,
    };

    #[tokio::test]
    async fn shutdown_task_join_reports_panics_and_cancellation() {
        let completed = tokio::spawn(async {});
        assert!(join_shutdown_task(completed, "completed").await.is_none());

        let unexpected_completion = shutdown_task_result(Ok(()), "unexpected", true).unwrap();
        assert!(unexpected_completion
            .to_string()
            .contains("unexpected task stopped unexpectedly"));

        let panicked = tokio::spawn(async { panic!("expected test panic") });
        let panic_error = join_shutdown_task(panicked, "panicked").await.unwrap();
        assert!(panic_error.to_string().contains("expected test panic"));

        let cancelled = tokio::spawn(std::future::pending::<()>());
        cancelled.abort();
        let cancel_error = join_shutdown_task(cancelled, "cancelled").await.unwrap();
        assert!(cancel_error.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn background_task_monitor_reports_failure_while_sibling_is_pending() {
        let mut panicked = tokio::spawn(async { panic!("relay panic") });
        let mut pending = tokio::spawn(std::future::pending::<()>());

        let exit = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_background_task_exit(&mut panicked, &mut pending),
        )
        .await
        .expect("background task monitor waited for the still-running sibling");

        match exit {
            BackgroundTaskExit::NotificationRelay(Some(error)) => {
                assert!(error.to_string().contains("relay panic"));
            }
            BackgroundTaskExit::HistoryWriter(_) => {
                panic!("monitor selected the still-running history writer")
            }
            BackgroundTaskExit::NotificationRelay(None) => {
                panic!("panic was treated as a successful completion")
            }
        }

        pending.abort();
        let _ = pending.await;
    }

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
    fn web_listen_address_defaults_and_accepts_overrides() {
        let default_args = super::Args::try_parse_from(["patrol"]).unwrap();
        assert_eq!(default_args.web_listen, "0.0.0.0:3000".parse().unwrap());

        let custom_args =
            super::Args::try_parse_from(["patrol", "--web-listen", "127.0.0.1:8080"]).unwrap();
        assert_eq!(custom_args.web_listen, "127.0.0.1:8080".parse().unwrap());
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
        let state = std::sync::Arc::new(WebAppState::new(
            events_rx,
            status_rx,
            tokio::sync::watch::channel(entries).1,
            tokio::sync::watch::channel(false).1,
        ));
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

        let response = reqwest::get(format!("http://{address}/api/v1/status"))
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "[]");

        let response = reqwest::get(format!("http://{address}/healthz"))
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "ok");

        let response = reqwest::get(format!("http://{address}/?event=unknown"))
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

        let response = reqwest::get(format!("http://{address}/ui")).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        let page = response.text().await.unwrap();
        assert!(page.contains("<dialog id=\"history-dialog\""));
        assert!(page.contains("id=\"history-retry\""));

        server.abort();
        let _ = server.await;
    }
}
