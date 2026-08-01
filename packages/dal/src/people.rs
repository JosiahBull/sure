use sqlx::FromRow;
use sure_core::{AppError, AppResult};
pub use sure_core::{Ownership, Person, SavePerson};

use crate::Db;

#[derive(Debug, FromRow)]
struct PersonRow {
    id: i64,
    name: String,
    color: Option<String>,
    sort_order: i64,
    placeholder: bool,
    created_at: String,
    updated_at: String,
}

impl From<PersonRow> for Person {
    fn from(r: PersonRow) -> Self {
        Person {
            id: r.id,
            name: r.name,
            color: r.color,
            sort_order: r.sort_order,
            placeholder: r.placeholder,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Person>> {
    Ok(sqlx::query_as::<_, PersonRow>(
        "SELECT * FROM people ORDER BY sort_order, name COLLATE NOCASE",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<Person> {
    Ok(
        sqlx::query_as::<_, PersonRow>("SELECT * FROM people WHERE id = ?1")
            .bind(id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("person"))?
            .into(),
    )
}

/// Whether a person id exists — for validating an [`Ownership::Person`] target before it
/// reaches the `accounts` FK, so the caller gets a 422 naming the field rather than a
/// bare constraint violation.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn exists(db: &Db, id: i64) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM people WHERE id = ?1")
            .bind(id)
            .fetch_one(db)
            .await?
            > 0,
    )
}

fn validate(input: &SavePerson) -> AppResult<()> {
    let mut problems = Vec::new();
    if input.name.trim().is_empty() {
        problems.push("name is required".to_string());
    }
    if let Some(color) = input
        .color
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        // The value is interpolated straight into the SPA's inline styles and chart fills, so
        // it has to be a colour and nothing else — a stray `;` would be a style injection.
        let valid = (color.len() == 7 || color.len() == 4)
            && color.starts_with('#')
            && color[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            problems.push(format!(
                "color '{color}' is not a hex colour (expected #rgb or #rrggbb)"
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::validation(problems.join("; ")))
    }
}

/// The colour as it should be stored: trimmed, lowercased, and blank normalised to `NULL`
/// so "cleared" is one value rather than two.
fn color_of(input: &SavePerson) -> Option<String> {
    input
        .color
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_lowercase)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SavePerson) -> AppResult<Person> {
    validate(&input)?;
    Ok(sqlx::query_as::<_, PersonRow>(
        "INSERT INTO people (name, color, sort_order) VALUES (?1, ?2, ?3) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(color_of(&input))
    .bind(input.sort_order)
    .fetch_one(db)
    .await
    .map_err(unique_name)?
    .into())
}

/// Rename or restyle someone.
///
/// A rename clears the `placeholder` flag: the flag records that *the app* invented this
/// person to satisfy the ownership requirement, and being given a real name is exactly the
/// answer it was standing in for. (Renaming it to "Unassigned" again is a choice, not the
/// app's assumption, so it still counts.)
#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SavePerson) -> AppResult<Person> {
    validate(&input)?;
    Ok(sqlx::query_as::<_, PersonRow>(
        "UPDATE people SET name=?2, color=?3, sort_order=?4, placeholder=0,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(color_of(&input))
    .bind(input.sort_order)
    .fetch_optional(db)
    .await
    .map_err(unique_name)?
    .ok_or(AppError::NotFound("person"))?
    .into())
}

/// Delete a person, refusing while anything is still attributed to them.
///
/// The schema's `ON DELETE RESTRICT` would refuse too, but as an opaque FK violation. A
/// person owning accounts is an ordinary thing to hit — you have to re-attribute or delete
/// those accounts first — so it answers with a 409 naming them, exactly like deleting an
/// asset that still has debts secured against it.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let owned = sqlx::query_scalar::<_, String>(
        "SELECT name FROM accounts WHERE person_id = ?1 ORDER BY sort_order, name",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    if !owned.is_empty() {
        return Err(AppError::conflict(format!(
            "Re-attribute or delete the accounts owned by this person first: {}",
            summarise(&owned)
        )));
    }
    // Every account must name an owner, so emptying the household would leave a database in
    // which no account can be created at all. Refused rather than allowed and then hit as a
    // baffling 422 on the next "add account".
    let remaining = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM people")
        .fetch_one(db)
        .await?;
    if remaining <= 1 {
        return Err(AppError::conflict(
            "The household needs at least one person — accounts have to belong to someone. \
             Add someone else first, or rename this one.",
        ));
    }
    let res = sqlx::query("DELETE FROM people WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("person"));
    }
    Ok(())
}

/// Name the first few and count the rest — a household with forty accounts shouldn't get
/// all forty names in an error toast.
fn summarise(names: &[String]) -> String {
    const SHOWN: usize = 5;
    if names.len() <= SHOWN {
        return names.join(", ");
    }
    format!(
        "{}, and {} more",
        names[..SHOWN].join(", "),
        names.len() - SHOWN
    )
}

// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
// (CLAUDE.md rule 2's escape hatch).
#[allow(clippy::wildcard_enum_match_arm)]
fn unique_name(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_unique_violation() => {
            AppError::conflict("someone in the household already has that name")
        }
        other => AppError::from(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    async fn person(db: &Db, name: &str) -> Person {
        create(
            db,
            SavePerson {
                name: name.to_string(),
                color: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn names_are_unique_case_insensitively() {
        let db = test_db().await;
        person(&db, "Alex").await;
        let err = create(
            &db,
            SavePerson {
                name: "alex".to_string(),
                color: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_colour_has_to_be_one() {
        let db = test_db().await;
        let err = create(
            &db,
            SavePerson {
                name: "Alex".to_string(),
                color: Some("red; background: url(x)".to_string()),
                sort_order: 0,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn blank_colour_stores_as_cleared() {
        let db = test_db().await;
        let p = create(
            &db,
            SavePerson {
                name: "Alex".to_string(),
                color: Some("   ".to_string()),
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(p.color, None);
    }

    #[tokio::test]
    async fn deleting_someone_who_owns_an_account_is_refused_by_name() {
        let db = test_db().await;
        let alex = person(&db, "Alex").await;
        sqlx::query(
            "INSERT INTO accounts (name, kind, currency_code, ownership, person_id)
             VALUES ('Everyday', 'bank', 'NZD', 'person', ?1)",
        )
        .bind(alex.id)
        .execute(&db)
        .await
        .unwrap();

        let err = delete(&db, alex.id).await.unwrap_err();
        let AppError::Conflict(message) = &err else {
            panic!("expected a conflict, got {err:?}");
        };
        assert!(message.contains("Everyday"), "got {message}");

        // ...and once nothing is attributed to them, it goes through. (The placeholder person
        // every database is created with is what keeps this from being the last one.)
        sqlx::query("UPDATE accounts SET ownership='joint', person_id=NULL")
            .execute(&db)
            .await
            .unwrap();
        delete(&db, alex.id).await.unwrap();
        assert!(!exists(&db, alex.id).await.unwrap());
    }
}
