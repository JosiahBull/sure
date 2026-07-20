use sure_server::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sure_api::init_tracing();
    let config = Config::from_env()?;
    sure_server::serve(config).await
}
