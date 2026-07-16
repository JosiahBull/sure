use std::net::SocketAddr;

/// Runtime configuration, sourced from the environment with sensible local defaults.
#[derive(Clone, Debug)]
pub struct Config {
    /// sqlx connection string, e.g. `sqlite:data/sure.db` or `sqlite::memory:`.
    pub database_url: String,
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Optional directory containing the built SPA. When set, the server serves it
    /// with SPA fallback so the whole app runs from a single binary in production.
    pub web_dir: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:data/sure.db".to_string());
        let bind_addr = std::env::var("BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
            .parse()?;
        let web_dir = std::env::var("WEB_DIR").ok().filter(|s| !s.is_empty());
        Ok(Self {
            database_url,
            bind_addr,
            web_dir,
        })
    }
}
