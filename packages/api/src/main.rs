use sure_api::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sure_api::init_tracing();
    let config = Config::from_env()?;
    sure_api::serve(config).await
}
