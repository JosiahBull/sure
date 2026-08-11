use sqlx::SqlitePool;
use sure_core::{AppError, AppResult, MAX_CATEGORY_DEPTH};
pub use sure_core::{Category, CategoryKind, CategoryNode, SaveCategory};

use crate::Db;

/// Parse a stored `kind` TEXT column into the domain enum, exactly like
/// `sure_dal::accounts::AccountRow`'s `TryFrom<AccountRow> for Account` does — every
/// writer goes through `CategoryKind::as_str`, so an unparseable value means the row
/// came from something else entirely and deserves a real error, not a silent default.
fn parse_kind(kind: String) -> AppResult<CategoryKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

#[derive(Debug)]
struct CategoryRow {
    id: i64,
    name: String,
    parent_id: Option<i64>,
    kind: String,
    color: Option<String>,
    icon: Option<String>,
    sort_order: i64,
    created_at: String,
}

impl TryFrom<CategoryRow> for Category {
    type Error = AppError;

    fn try_from(r: CategoryRow) -> AppResult<Self> {
        Ok(Category {
            kind: parse_kind(r.kind)?,
            id: r.id,
            name: r.name,
            parent_id: r.parent_id,
            color: r.color,
            icon: r.icon,
            sort_order: r.sort_order,
            created_at: r.created_at,
        })
    }
}

/// List all categories (flat).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Category>> {
    all_categories(db).await
}

/// The category tree (roots with nested children).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn tree(db: &Db) -> AppResult<Vec<CategoryNode>> {
    let flat = all_categories(db).await?;
    Ok(build_tree(flat))
}

/// Create a category.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveCategory) -> AppResult<Category> {
    validate(db, &input, None).await?;
    let name = input.name.trim();
    let kind = input.kind.as_str();
    sqlx::query_as!(
        CategoryRow,
        r#"INSERT INTO categories (name, parent_id, kind, color, icon, sort_order)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)
           RETURNING id AS "id!", name, parent_id AS "parent_id?", kind, color, icon,
                     sort_order, created_at"#,
        name,
        input.parent_id,
        kind,
        input.color,
        input.icon,
        input.sort_order
    )
    .fetch_one(db)
    .await?
    .try_into()
}

/// Replace a category.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveCategory) -> AppResult<Category> {
    validate(db, &input, Some(id)).await?;
    let name = input.name.trim();
    let kind = input.kind.as_str();
    sqlx::query_as!(
        CategoryRow,
        r#"UPDATE categories SET name=?2, parent_id=?3, kind=?4, color=?5, icon=?6, sort_order=?7
           WHERE id=?1
           RETURNING id AS "id!", name, parent_id AS "parent_id?", kind, color, icon,
                     sort_order, created_at"#,
        id,
        name,
        input.parent_id,
        kind,
        input.color,
        input.icon,
        input.sort_order
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("category"))?
    .try_into()
}

/// Find an existing category by name (case-insensitive, scoped to the given parent) or
/// create one. Used by provider imports to reuse a source's own classification (e.g.
/// Akahu's NZFCC categories) without duplicating a category on every sync. There's no
/// uniqueness constraint on `(name, parent_id)` — categories are otherwise entirely
/// user-managed — so this is a plain check-then-insert, not an atomic upsert.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn find_or_create(
    db: &Db,
    name: &str,
    parent_id: Option<i64>,
    kind: CategoryKind,
) -> AppResult<Category> {
    let name = name.trim();
    // One statement rather than the two the untyped version needed: `IS` (not `=`) matches a
    // NULL bind against a NULL `parent_id`, so the top-level lookup is the same query with a
    // NULL parameter — and the macro needs a single literal string either way.
    let existing: Option<Category> = sqlx::query_as!(
        CategoryRow,
        r#"SELECT id AS "id!", name, parent_id, kind, color, icon, sort_order, created_at
             FROM categories WHERE name = ?1 COLLATE NOCASE AND parent_id IS ?2"#,
        name,
        parent_id
    )
    .fetch_optional(db)
    .await?
    .map(Category::try_from)
    .transpose()?;
    if let Some(existing) = existing {
        return Ok(existing);
    }
    let kind = kind.as_str();
    sqlx::query_as!(
        CategoryRow,
        r#"INSERT INTO categories (name, parent_id, kind) VALUES (?1, ?2, ?3)
           RETURNING id AS "id!", name, parent_id AS "parent_id?", kind, color, icon,
                     sort_order, created_at"#,
        name,
        parent_id,
        kind
    )
    .fetch_one(db)
    .await?
    .try_into()
}

/// Delete a category. Child categories and transaction links cascade per schema
/// (`ON DELETE CASCADE` for children, `SET NULL` for transactions).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM categories WHERE id = ?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("category"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn all_categories(db: &SqlitePool) -> AppResult<Vec<Category>> {
    sqlx::query_as!(
        CategoryRow,
        r#"SELECT id AS "id!", name, parent_id, kind, color, icon, sort_order, created_at
             FROM categories ORDER BY sort_order, name"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Category::try_from)
    .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
async fn validate(db: &SqlitePool, input: &SaveCategory, id: Option<i64>) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("category name is required"));
    }
    if let Some(parent) = input.parent_id {
        if Some(parent) == id {
            return Err(AppError::validation("a category cannot be its own parent"));
        }
        // Parent must exist.
        let exists = sqlx::query_scalar!("SELECT COUNT(*) FROM categories WHERE id = ?1", parent)
            .fetch_one(db)
            .await?;
        if exists == 0 {
            return Err(AppError::validation("parent category does not exist"));
        }
        // Prevent cycles: the proposed parent must not be a descendant of this node.
        if let Some(id) = id
            && would_cycle(db, id, parent).await?
        {
            return Err(AppError::validation(
                "cannot nest a category under one of its own descendants",
            ));
        }
        // Depth cap, in two halves because a re-parent takes a whole subtree with it:
        // where this node itself would land, and where its deepest descendant would. A
        // check on the parent alone would happily accept moving a 2-deep branch under a
        // depth-1 category. Only reached when a parent is set — moving to the top level
        // can only make the tree shallower. Runs after `would_cycle` so a cyclic chain is
        // rejected before either recursive walk below has to cope with it.
        let height = match id {
            Some(id) => subtree_height(db, id).await?,
            None => 0, // a category being created has no children yet
        };
        if depth_of(db, parent).await? + 1 + height > MAX_CATEGORY_DEPTH - 1 {
            return Err(AppError::validation(format!(
                "categories nest at most {MAX_CATEGORY_DEPTH} levels deep"
            )));
        }
    }
    Ok(())
}

/// How deep `id` sits: 0 for a top-level category.
///
/// Bounded at 64 hops, mirroring the guard in `sure_app::reports::Categories` — `validate`
/// runs `would_cycle` first so a cycle can't reach here through the API, but a recursive
/// CTE over a hand-edited cyclic parent chain would otherwise spin forever.
#[tracing::instrument(level = "debug", skip_all)]
async fn depth_of(db: &SqlitePool, id: i64) -> AppResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE up(id, parent_id, depth) AS (
               SELECT id, parent_id, 0 FROM categories WHERE id = ?1
               UNION ALL
               SELECT c.id, c.parent_id, up.depth + 1
               FROM categories c JOIN up ON c.id = up.parent_id WHERE up.depth < 64
           )
           SELECT COALESCE(MAX(depth), 0) AS "depth!: i64" FROM up"#,
        id
    )
    .fetch_one(db)
    .await?)
}

/// How many levels sit *below* `id`: 0 for a leaf. Same 64-hop bound as [`depth_of`].
#[tracing::instrument(level = "debug", skip_all)]
async fn subtree_height(db: &SqlitePool, id: i64) -> AppResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE down(id, depth) AS (
               SELECT id, 0 FROM categories WHERE id = ?1
               UNION ALL
               SELECT c.id, down.depth + 1
               FROM categories c JOIN down ON c.parent_id = down.id WHERE down.depth < 64
           )
           SELECT COALESCE(MAX(depth), 0) AS "depth!: i64" FROM down"#,
        id
    )
    .fetch_one(db)
    .await?)
}

#[tracing::instrument(level = "debug", skip_all)]
async fn would_cycle(db: &SqlitePool, id: i64, parent: i64) -> AppResult<bool> {
    let mut cursor = Some(parent);
    while let Some(current) = cursor {
        if current == id {
            return Ok(true);
        }
        cursor = sqlx::query_scalar!("SELECT parent_id FROM categories WHERE id=?1", current)
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
            node.children = kids
                .iter()
                .map(|&k| assemble(k, nodes, children_of))
                .collect();
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
