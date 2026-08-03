use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A category's flow direction. Stored as `categories.kind` (a plain `TEXT` column).
/// Transfers are excluded from spend reports (see `sure_app::reports::is_transfer`).
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum CategoryKind {
    Income,
    /// Most enrichment (a category created from a provider's own classification, or a
    /// blank new-category form) is spend-side, so this is the default.
    #[default]
    Expense,
    /// Internal money movement (a wallet ↔ bank transfer, funding a trade), excluded
    /// from income/expense reports.
    Transfer,
}

impl CategoryKind {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        use CategoryKind::*;
        match self {
            Income => "income",
            Expense => "expense",
            Transfer => "transfer",
        }
    }
}

impl std::str::FromStr for CategoryKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use CategoryKind::*;
        Ok(match s {
            "income" => Income,
            "expense" => Expense,
            "transfer" => Transfer,
            other => return Err(format!("unknown category kind '{other}'")),
        })
    }
}

/// How many levels of category nesting the app supports, top level included — so
/// `Income > Employment > Partly Group` is the deepest legal chain.
///
/// The money-flow report draws exactly this many columns per side and the category
/// pickers qualify a name with its ancestors, so a deeper tree could be built but never
/// fully shown. Enforced on the CRUD path only (`sure_dal::categories::validate`): the
/// provider imports' `find_or_create` and the snapshot restore write rows directly, so a
/// deeper tree can still reach the reports and they have to roll it up rather than assume
/// the cap holds.
pub const MAX_CATEGORY_DEPTH: i64 = 3;

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct Category {
    pub id: i64,
    pub name: String,
    /// Parent category for nesting; `null` for a top-level category.
    pub parent_id: Option<i64>,
    pub kind: CategoryKind,
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
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryNode {
    pub category: Category,
    #[schema(no_recursion)]
    pub children: Vec<CategoryNode>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveCategory {
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub kind: CategoryKind,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}
