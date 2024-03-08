use std::future::IntoFuture;
use std::sync::mpsc;

use clap::Parser;
use env_logger::Env;
use log::{error, info};

use patrol::application::{App, SelectivePoller};
use patrol::infrastructure::{
    HttpPoller, TomlConfigRepository, TomlDataRepository, WebDriverPoller,
};
use tokio::io::AsyncBufReadExt;
use tokio::sync::oneshot;

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
    let simple_modepoller = HttpPoller::new();

    let poller = SelectivePoller::new(full_mode_poller, simple_modepoller);

    let interval_period_secs = args.interval_minutes.max(1) as u64 * 60;
    let interval_limit = if args.once { Some(1) } else { None };

    info!("start app.");
    let app = App::new(
        config_repo,
        data_repo,
        poller,
        interval_period_secs,
        interval_limit,
    );

    let (tx, rx) = oneshot::channel();
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

        tx.send(())
    });

    tokio::select! {
        _quit = rx  => (),
        result = app.run() => {
            if let Err(why) = result {
                error!("{why}")
            }
        },
    }

    Ok(())
}
