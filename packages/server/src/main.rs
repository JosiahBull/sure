use sure_server::config::{load_dotenv, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First of all: `init_tracing` reads `RUST_LOG`, and the provider clients read their
    // tokens lazily on the first sync — both have to see whatever the file sets. That
    // costs the log line a subscriber, so the path is reported once there is one.
    let env_file = load_dotenv()?;
    sure_api::init_tracing();
    if let Some(path) = &env_file {
        tracing::info!(file = %path.display(), "loaded env file");
    }
    let config = Config::from_env()?;
    sure_server::serve(config).await
}
