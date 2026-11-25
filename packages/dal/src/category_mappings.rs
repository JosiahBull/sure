use sure_core::{AppError, AppResult, CategoryMapping, SaveCategoryMapping};

use crate::Db;

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<CategoryMapping>> {
    Ok(sqlx::query_as!(
        CategoryMapping,
        r#"SELECT id AS "id!", source, source_group, source_name, category_id,
                  created_at, updated_at
             FROM category_mappings ORDER BY source, source_group, source_name"#
    )
    .fetch_all(db)
    .await?)
}

async fn validate(db: &Db, input: &SaveCategoryMapping) -> AppResult<()> {
    // Adapter keys are open identifiers: the provider registry can add new sources.
    let source = input.source.trim();
    if source.is_empty()
        || source.len() > 80
        || !source
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(AppError::validation(
            "source must be a source key such as akahu or asb",
        ));
    }
    if input.source_name.trim().is_empty()
        || input.source_name.chars().count() > 200
        || input.source_group.chars().count() > 200
    {
        return Err(AppError::validation(
            "source category is required; category and group must be at most 200 characters",
        ));
    }
    if let Some(id) = input.category_id {
        let exists = sqlx::query_scalar!("SELECT COUNT(*) FROM categories WHERE id = ?1", id)
            .fetch_one(db)
            .await?;
        if exists == 0 {
            return Err(AppError::validation("destination category does not exist"));
        }
    }
    Ok(())
}

fn write_error(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .is_some_and(|e| e.is_unique_violation())
    {
        AppError::validation("a mapping for this source category already exists")
    } else if error
        .as_database_error()
        .is_some_and(|e| e.is_foreign_key_violation())
    {
        AppError::validation("destination category does not exist")
    } else {
        error.into()
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveCategoryMapping) -> AppResult<CategoryMapping> {
    validate(db, &input).await?;
    let source = input.source.trim().to_ascii_lowercase();
    let group = input.source_group.trim();
    let name = input.source_name.trim();
    sqlx::query_as!(
        CategoryMapping,
        r#"INSERT INTO category_mappings (source, source_group, source_name, category_id)
           VALUES (?1, ?2, ?3, ?4)
           RETURNING id AS "id!", source, source_group, source_name, category_id AS "category_id?",
                     created_at, updated_at"#,
        source,
        group,
        name,
        input.category_id
    )
    .fetch_one(db)
    .await
    .map_err(write_error)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveCategoryMapping) -> AppResult<CategoryMapping> {
    validate(db, &input).await?;
    let source = input.source.trim().to_ascii_lowercase();
    let group = input.source_group.trim();
    let name = input.source_name.trim();
    sqlx::query_as!(
        CategoryMapping,
        r#"UPDATE category_mappings SET source=?2, source_group=?3, source_name=?4,
                  category_id=?5, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
            WHERE id=?1
            RETURNING id AS "id!", source, source_group, source_name, category_id AS "category_id?",
                      created_at, updated_at"#,
        id,
        source,
        group,
        name,
        input.category_id
    )
    .fetch_optional(db)
    .await
    .map_err(write_error)?
    .ok_or(AppError::NotFound("category mapping"))
}

/// Remember newly observed labels once. An existing mapping, including a cleared one, wins.
/// Source labels never create categories. This lookup is cached by the import batch.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn resolve(
    db: &Db,
    source: &str,
    group: Option<&str>,
    name: &str,
) -> AppResult<Option<i64>> {
    let source = source.trim().to_ascii_lowercase();
    let group = group.unwrap_or("").trim();
    let name = name.trim();
    let existing = sqlx::query!(
        "SELECT category_id FROM category_mappings WHERE source=?1 AND source_group=?2 AND source_name=?3",
        source, group, name
    ).fetch_optional(db).await?;
    if let Some(row) = existing {
        return Ok(row.category_id);
    }
    let parent = if group.is_empty() {
        None
    } else {
        crate::categories::find(db, group, None)
            .await?
            .map(|c| c.id)
    };
    let destination = if !group.is_empty() && parent.is_none() {
        None
    } else {
        crate::categories::find(db, name, parent)
            .await?
            .map(|c| c.id)
    };
    // A concurrent import or a user saving this label must not be overwritten.
    sqlx::query!(
        "INSERT INTO category_mappings (source, source_group, source_name, category_id) VALUES (?1,?2,?3,?4) ON CONFLICT(source,source_group,source_name) DO NOTHING",
        source, group, name, destination
    ).execute(db).await?;
    Ok(sqlx::query!(
        "SELECT category_id FROM category_mappings WHERE source=?1 AND source_group=?2 AND source_name=?3",
        source, group, name
    ).fetch_one(db).await?.category_id)
}

/// Complete only the fallbacks captured by a new import, after explicit rules had a chance.
/// Consuming the pending value prevents later mapping edits from backfilling old transactions.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn apply_pending(db: &Db, ids: &[i64]) -> AppResult<()> {
    let ids = serde_json::to_string(ids).map_err(|e| AppError::Internal(e.into()))?;
    sqlx::query!(
        r#"UPDATE transactions SET category_id=COALESCE(category_id,pending_category_id),
                  pending_category_id=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
            WHERE pending_category_id IS NOT NULL
              AND (category_id IS NOT NULL OR id IN (SELECT value FROM json_each(?1)))"#,
        ids
    )
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        let db = crate::connect("sqlite::memory:").await.unwrap();
        crate::migrate(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn disabled_and_deleted_destinations_are_not_restored_by_observation() {
        let db = db().await;
        let mut mapping = list(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.source_name == "Cafes and restaurants")
            .unwrap();
        let original = mapping.category_id.unwrap();
        let destination = crate::categories::create(
            &db,
            sure_core::SaveCategory {
                name: "Coffee budget".into(),
                parent_id: None,
                kind: sure_core::CategoryKind::Expense,
                color: None,
                icon: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        mapping = update(
            &db,
            mapping.id,
            SaveCategoryMapping {
                source: mapping.source,
                source_group: mapping.source_group,
                source_name: mapping.source_name,
                category_id: Some(destination.id),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            resolve(&db, "AKAHU", Some("lifestyle"), "CAFES AND RESTAURANTS")
                .await
                .unwrap(),
            Some(destination.id)
        );
        crate::categories::delete(&db, destination.id)
            .await
            .unwrap();
        assert!(
            crate::categories::list(&db)
                .await
                .unwrap()
                .iter()
                .any(|c| c.id == original)
        );
        assert_eq!(
            resolve(&db, "akahu", Some("Lifestyle"), "Cafes and restaurants")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            list(&db)
                .await
                .unwrap()
                .iter()
                .find(|m| m.id == mapping.id)
                .unwrap()
                .category_id,
            None
        );
        // A different source's identical label has independent configuration.
        assert_eq!(
            resolve(
                &db,
                "another-feed",
                Some("Lifestyle"),
                "Cafes and restaurants"
            )
            .await
            .unwrap(),
            Some(original)
        );
    }

    #[tokio::test]
    async fn an_upgrade_maps_missing_original_labels_to_existing_summary_categories() {
        let db = crate::connect("sqlite::memory:").await.unwrap();
        let previous = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                crate::MIGRATOR
                    .iter()
                    .filter(|m| m.version < 57)
                    .cloned()
                    .collect(),
            ),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        previous.run(&db).await.unwrap();
        let lifestyle = crate::categories::find(&db, "Lifestyle", None)
            .await
            .unwrap()
            .unwrap();
        crate::categories::delete(&db, lifestyle.id).await.unwrap();
        let summary = crate::categories::create(
            &db,
            sure_core::SaveCategory {
                name: "Eating out".into(),
                parent_id: None,
                kind: sure_core::CategoryKind::Expense,
                color: None,
                icon: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        let before = crate::categories::list(&db).await.unwrap().len();
        crate::migrate(&db).await.unwrap();
        assert_eq!(
            resolve(&db, "akahu", Some("Lifestyle"), "Cafes and restaurants")
                .await
                .unwrap(),
            Some(summary.id)
        );
        assert_eq!(
            resolve(&db, "akahu", Some("Lifestyle"), "Fast food stores")
                .await
                .unwrap(),
            Some(summary.id)
        );
        assert_eq!(crate::categories::list(&db).await.unwrap().len(), before);
    }
}
