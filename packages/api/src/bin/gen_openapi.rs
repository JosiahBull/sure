//! Emit the OpenAPI document as JSON, either to a path (arg 1) or stdout.
//! Drives TypeScript client generation without needing a running server.
use std::path::Path;

use sure_api::openapi::ApiDoc;
use utoipa::OpenApi;

fn main() -> anyhow::Result<()> {
    let json = ApiDoc::openapi().to_pretty_json()?;
    match std::env::args().nth(1) {
        Some(path) => {
            if let Some(parent) = Path::new(&path).parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, json)?;
            eprintln!("wrote OpenAPI spec to {path}");
        }
        None => println!("{json}"),
    }
    Ok(())
}
