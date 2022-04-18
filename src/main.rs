use clap::Parser;
use env_logger::Env;
use log::error;

use patrol::application::{App, SelectivePoller};
use patrol::infrastructure::{
    HttpPoller, TomlConfigRepository, TomlDataRepository, WebDriverPoller,
};

#[derive(Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(
        short,
        long,
        help = r#"Specify the config file\nIf omitted, "./config.toml" is used."#,
        default_value = "./config.toml"
    )]
    config_path: String,
    #[clap(
        short,
        long,
        help = r#"Specify the data file\nIf omitted, "./data.toml" is used."#,
        default_value = "./data.toml"
    )]
    data_path: String,
    #[clap(
        short('p'),
        long,
        help = r#"Specify the Web Driver port to connect to\nIf omitted, 9515 is used."#,
        default_value_t = 9515
    )]
    webdriver_port: u16,
    #[clap(
        short('i'),
        long,
        help = r#"Specify the patrol interval in minutes\nIf omitted, 1 is used."#,
        default_value_t = 1
    )]
    interval_minutes: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    env_logger::Builder::from_env(Env::default().default_filter_or("warn")).init();

    let config_repo = TomlConfigRepository::new(&args.config_path).await?;
    let data_repo = TomlDataRepository::new(&args.data_path).await?;

    let full_mode_poller = WebDriverPoller::new(args.webdriver_port).await?;
    let simple_modepoller = HttpPoller::new();

    let poller = SelectivePoller::new(full_mode_poller, simple_modepoller);

    let interval_period_secs = args.interval_minutes.max(1) as u64 * 60;

    let app = App::new(config_repo, data_repo, poller, interval_period_secs);

    if let Err(why) = app.run().await {
        error!("{why}")
    }

    Ok(())
}
