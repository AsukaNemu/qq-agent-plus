use std::path::PathBuf;

use clap::Parser;

use crate::application::router::DEFAULT_MESSAGE_QUEUE_CAPACITY;

#[derive(Clone, Debug, Parser)]
#[command(name = "RInBot", about = "RInBot Rust business core")]
pub struct Cli {
    #[arg(long, env = "QQBOTD_HOST", default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, env = "QQBOTD_PORT", default_value_t = 3211)]
    pub port: u16,
    #[arg(long, env = "QQBOTD_DATA_DIR", default_value = "var")]
    pub data_dir: PathBuf,
    #[arg(long, env = "QQBOTD_UI_DIR", default_value = "ui/dist")]
    pub ui_dir: PathBuf,
    #[arg(
        long,
        env = "QQBOTD_ONEBOT_WS_URL",
        default_value = "ws://127.0.0.1:3001/onebot/v11/ws"
    )]
    pub onebot_ws_url: String,
    #[arg(
        long,
        env = "QQBOTD_ONEBOT_HTTP_URL",
        default_value = "http://127.0.0.1:3000"
    )]
    pub onebot_http_url: String,
    #[arg(long, env = "QQBOTD_ONEBOT_TOKEN", default_value = "")]
    pub onebot_token: String,
    #[arg(long, env = "QQBOTD_API_TOKEN", default_value = "")]
    pub api_token: String,
    #[arg(long, env = "QQBOTD_CONNECT_ONEBOT", default_value_t = false)]
    pub connect_onebot: bool,
    #[arg(long, default_value_t = DEFAULT_MESSAGE_QUEUE_CAPACITY)]
    pub queue_capacity: usize,
}
