use sure_dal::Db;

/// Shared application state handed to every handler. Cheap to clone (the pool is an
/// `Arc` internally). The DB handle is the DAL's `Db` type, so the API crate never
/// names `sqlx` directly.
#[derive(Debug, Clone)]
pub struct AppState {
    pub db: Db,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}
