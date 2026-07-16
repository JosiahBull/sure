use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use sure_core::{AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

const KINDS: [&str; 3] = ["income", "expense", "transfer"];

#[derive(Serialize, FromRow, ToSchema, Clone)]
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

/// List all categories (flat).
pub async fn list(db: &Db) -> AppResult<Vec<Category>> {
    all_categories(db).await
}

/// The category tree (roots with nested children).
pub async fn tree(db: &Db) -> AppResult<Vec<CategoryNode>> {
    let flat = all_categories(db).await?;
    Ok(build_tree(flat))
}

/// Create a category.
pub async fn create(db: &Db, input: SaveCategory) -> AppResult<Category> {
    validate(db, &input, None).await?;
    Ok(sqlx::query_as::<_, Category>(
        "INSERT INTO categories (name, parent_id, kind, color, icon, sort_order)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(input.parent_id)
    .bind(&input.kind)
    .bind(&input.color)
    .bind(&input.icon)
    .bind(input.sort_order)
    .fetch_one(db)
    .await?)
}

/// Replace a category.
pub async fn update(db: &Db, id: i64, input: SaveCategory) -> AppResult<Category> {
    validate(db, &input, Some(id)).await?;
    sqlx::query_as::<_, Category>(
        "UPDATE categories SET name=?2, parent_id=?3, kind=?4, color=?5, icon=?6, sort_order=?7
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(input.parent_id)
    .bind(&input.kind)
    .bind(&input.color)
    .bind(&input.icon)
    .bind(input.sort_order)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("category"))
}

/// Delete a category. Child categories and transaction links cascade per schema
/// (`ON DELETE CASCADE` for children, `SET NULL` for transactions).
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM categories WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("category"));
    }
    Ok(())
}

async fn all_categories(db: &SqlitePool) -> AppResult<Vec<Category>> {
    Ok(
        sqlx::query_as::<_, Category>("SELECT * FROM categories ORDER BY sort_order, name")
            .fetch_all(db)
            .await?,
    )
}

async fn validate(db: &SqlitePool, input: &SaveCategory, id: Option<i64>) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("category name is required"));
    }
    if !KINDS.contains(&input.kind.as_str()) {
        return Err(AppError::validation(format!(
            "kind must be one of {KINDS:?}"
        )));
    }
    if let Some(parent) = input.parent_id {
        if Some(parent) == id {
            return Err(AppError::validation("a category cannot be its own parent"));
        }
        // Parent must exist.
        let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM categories WHERE id = ?1")
            .bind(parent)
            .fetch_one(db)
            .await?;
        if exists == 0 {
            return Err(AppError::validation("parent category does not exist"));
        }
        // Prevent cycles: the proposed parent must not be a descendant of this node.
        if let Some(id) = id {
            if would_cycle(db, id, parent).await? {
                return Err(AppError::validation(
                    "cannot nest a category under one of its own descendants",
                ));
            }
        }
    }
    Ok(())
}

async fn would_cycle(db: &SqlitePool, id: i64, parent: i64) -> AppResult<bool> {
    let mut cursor = Some(parent);
    while let Some(current) = cursor {
        if current == id {
            return Ok(true);
        }
        cursor = sqlx::query_scalar::<_, Option<i64>>("SELECT parent_id FROM categories WHERE id=?1")
            .bind(current)
            .fetch_optional(db)
            .await?
            .flatten();
    }
    Ok(false)
}

/// Assemble a flat, ordered category list into a forest, preserving order.
fn build_tree(flat: Vec<Category>) -> Vec<CategoryNode> {
    use std::collections::HashMap;
    // Record child order per parent while keeping node data addressable by id.
    let mut nodes: HashMap<i64, CategoryNode> = HashMap::with_capacity(flat.len());
    let mut order: Vec<i64> = Vec::with_capacity(flat.len());
    let mut children_of: HashMap<Option<i64>, Vec<i64>> = HashMap::new();
    for c in flat {
        order.push(c.id);
        children_of.entry(c.parent_id).or_default().push(c.id);
        nodes.insert(
            c.id,
            CategoryNode {
                category: c,
                children: Vec::new(),
            },
        );
    }
    // Build bottom-up so a moved node carries its own subtree. Process ids in reverse
    // of insertion isn't safe for arbitrary depth, so recurse from roots instead.
    fn assemble(
        id: i64,
        nodes: &mut HashMap<i64, CategoryNode>,
        children_of: &HashMap<Option<i64>, Vec<i64>>,
    ) -> CategoryNode {
        let mut node = nodes.remove(&id).expect("node present");
        if let Some(kids) = children_of.get(&Some(id)) {
            node.children = kids.iter().map(|&k| assemble(k, nodes, children_of)).collect();
        }
        node
    }
    children_of
        .get(&None)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|id| assemble(id, &mut nodes, &children_of))
        .collect()
}
