use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema, Clone)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Category {
    pub id: i64,
    pub name: String,
    /// Parent category for nesting; `null` for a top-level category.
    pub parent_id: Option<i64>,
    /// `income` | `expense` | `transfer`. Transfers are excluded from spend reports.
    pub kind: String,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
}

/// A category plus its nested children, for rendering the category tree.
///
/// `#[schema(no_recursion)]` is required: this type is self-referential via
/// `children`, and utoipa 5 eagerly inlines nested schemas by default, so without it
/// building the OpenAPI document recurses forever and overflows the stack. The
/// attribute tells utoipa to emit a `$ref` for the recursive field and terminate.
#[derive(Serialize, ToSchema)]
pub struct CategoryNode {
    pub category: Category,
    #[schema(no_recursion)]
    pub children: Vec<CategoryNode>,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveCategory {
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}

fn default_kind() -> String {
    "expense".to_string()
}
