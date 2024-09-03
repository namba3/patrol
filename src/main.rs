use clap::Parser;
use env_logger::Env;
use futures::SinkExt;
use log::{error, info};

use patrol::application::app::DocUpdateInfo;
use patrol::application::{App, SelectivePoller};
use patrol::infrastructure::{
    HttpPoller, TomlConfigRepository, TomlDataRepository, WebDriverPoller,
};

use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        Extension,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures::stream::StreamExt;
use serde::Serialize;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::sync::{broadcast, oneshot};
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
        help = "Specify the Web Driver port to connect to.\nThis can be specified multiple times.",
        default_value = "9515"
    )]
    webdriver_ports: Vec<u16>,
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
    info!("webdriver_ports:  {:?}", args.webdriver_ports);

    let config_repo = TomlConfigRepository::new(&args.config_path).await?;
    let data_repo = TomlDataRepository::new(&args.data_path).await?;

    let full_mode_poller = WebDriverPoller::new(args.webdriver_ports.as_slice()).await?;
    let simple_mode_poller = HttpPoller::new();

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

    let (tx_doc_update, mut rx_doc_update) =
        tokio::sync::mpsc::unbounded_channel::<DocUpdateInfo>();
    let (tx, rx) = broadcast::channel(100);
    let web_app_state = Arc::new(AppState { rx });
    let web_app = Router::new()
        .route("/", get(websocket_handler))
        .layer(Extension(web_app_state));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;

    let web_app = async { axum::serve(listener, web_app).await };

    let message_dealer = tokio::spawn(async move {
        while let Some(x) = rx_doc_update.recv().await {
            let msg = Message {
                id: x.id,
                url: x.url,
                timestamp: x.timestamp,
            };
            let msg = serde_json::to_string(&msg).unwrap();

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

        tx_command.send(());
    });

    tokio::select! {
        _quit = rx_command => (),
        result = web_app  => {
            if let Err(why) = result {
                error!("{why}")
            }
        },
        result = patrol_app.run(tx_doc_update) => {
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
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(stream: WebSocket, state: Arc<AppState>) {
    let (mut sender, _receiver) = stream.split();

    let mut rx = state.rx.resubscribe();

    while let Ok(msg) = rx.recv().await {
        if let Err(why) = sender.send(msg.into()).await {
            log::warn!("{why}")
        }
    }
}

#[derive(Serialize)]
struct Message {
    pub id: String,
    pub url: String,
    pub timestamp: String,
}
