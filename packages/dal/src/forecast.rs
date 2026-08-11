//! Forecast assumption overrides: persistence. The resolution logic (which knob wins)
//! lives in `sure-app`; this module only stores/retrieves the override rows plus the one
//! read query (`trailing_dividends_minor`) nothing else already exposes.

use sure_core::{
    AppError, AppResult, EffectColumns, ForecastAssumption, ForecastEvent, ForecastEventEffect,
    ForecastEventRelation, ForecastTargetType, LifeEffectKind, LifeEffectSpec, RelationKind,
    SaveForecastAssumption, SaveForecastEvent, SaveForecastEventRelation,
};

use crate::Db;

#[derive(Debug)]
struct ForecastAssumptionRow {
    id: i64,
    target_type: String,
    target_id: i64,
    annual_growth_bps: Option<i64>,
    annual_volatility_bps: Option<i64>,
    dividend_yield_bps: Option<i64>,
    long_run_growth_bps: Option<i64>,
    annual_fee_bps: Option<i64>,
    annual_fixed_fee_minor: Option<i64>,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<ForecastAssumptionRow> for ForecastAssumption {
    type Error = AppError;

    fn try_from(r: ForecastAssumptionRow) -> AppResult<Self> {
        // Every writer goes through `ForecastTargetType::as_str`, so a value that
        // doesn't parse means the row was written by something else entirely — surface
        // it as a real error rather than panicking the request.
        let target_type: ForecastTargetType = r
            .target_type
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(ForecastAssumption {
            id: r.id,
            target_type,
            target_id: r.target_id,
            annual_growth_bps: r.annual_growth_bps,
            annual_volatility_bps: r.annual_volatility_bps,
            dividend_yield_bps: r.dividend_yield_bps,
            long_run_growth_bps: r.long_run_growth_bps,
            annual_fee_bps: r.annual_fee_bps,
            annual_fixed_fee_minor: r.annual_fixed_fee_minor,
            notes: r.notes,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// Every stored override, across both account and category targets.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_assumptions(db: &Db) -> AppResult<Vec<ForecastAssumption>> {
    sqlx::query_as!(
        ForecastAssumptionRow,
        r#"SELECT id AS "id!", target_type, target_id, annual_growth_bps, annual_volatility_bps,
                  dividend_yield_bps, long_run_growth_bps, annual_fee_bps,
                  annual_fixed_fee_minor, notes, created_at, updated_at
             FROM forecast_assumptions ORDER BY id"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect()
}

/// Ceiling on an explicit `annual_volatility_bps` override, 300%/yr in basis points.
///
/// Volatility is the standard deviation of a lognormal monthly draw, so it sets the
/// *exponent* the simulation raises `e` to: `exp()` saturates past ±745, and an
/// `annual_volatility_bps` in the millions makes both an underflow to `0.0` and an overflow
/// to `inf` routine within a single path. `0.0 * inf` is `NaN`, `NaN >= 0.0` is false so it
/// files itself as a liability, and it reaches the percentile bands — which used to sort with
/// `partial_cmp().unwrap()` and panic. `CatchPanicLayer` turned that into a 500 on
/// `GET /api/forecast` that persisted until the offending row was deleted, through an endpoint
/// on the page that was down.
///
/// So it is rejected here rather than silently clamped: the user typed a number and is told
/// it is out of range, instead of getting a projection quietly computed from a different one.
/// `sure_app::forecast` clamps the same value at the use site regardless — that clamp is the
/// last line of defence for a row written before this validation existed, and for one written
/// by anything that isn't this function. The bound matches
/// `sure_app::forecast::MAX_DERIVED_CATEGORY_VOL_BPS`, which is a numerical guard rather than
/// an opinion about how lumpy a real series can be: measured category volatilities do reach
/// several hundred percent and that is a true description of them.
const MAX_VOLATILITY_BPS: i64 = 30_000;

/// Insert or replace the override for `(target_type, target_id)`. A `None` field clears
/// that knob back to "derive from history" — this replaces the whole row, it doesn't
/// patch individual fields.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert_assumption(
    db: &Db,
    input: SaveForecastAssumption,
) -> AppResult<ForecastAssumption> {
    // Validated on the way in, like `create_event` below: nothing downstream can tell a
    // deliberate 1e14 from a fat-fingered one, and by the time it reaches the simulation the
    // only honest options left are clamping it (a projection of an assumption the user never
    // made) or refusing the whole report.
    if let Some(vol) = input.annual_volatility_bps
        && !(0..=MAX_VOLATILITY_BPS).contains(&vol)
    {
        return Err(AppError::validation(format!(
            "annual_volatility_bps must be between 0 and {MAX_VOLATILITY_BPS} \
                 (0-300%/yr), got {vol}"
        )));
    }
    let target_type = input.target_type.as_str();
    sqlx::query_as!(
        ForecastAssumptionRow,
        r#"INSERT INTO forecast_assumptions
              (target_type, target_id, annual_growth_bps, annual_volatility_bps,
               dividend_yield_bps, long_run_growth_bps, notes, annual_fee_bps,
               annual_fixed_fee_minor)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
           ON CONFLICT(target_type, target_id) DO UPDATE SET
              annual_growth_bps=excluded.annual_growth_bps,
              annual_volatility_bps=excluded.annual_volatility_bps,
              dividend_yield_bps=excluded.dividend_yield_bps,
              long_run_growth_bps=excluded.long_run_growth_bps,
              annual_fee_bps=excluded.annual_fee_bps,
              annual_fixed_fee_minor=excluded.annual_fixed_fee_minor,
              notes=excluded.notes,
              updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
           RETURNING id AS "id!", target_type, target_id, annual_growth_bps, annual_volatility_bps,
                     dividend_yield_bps, long_run_growth_bps,
                     annual_fee_bps AS "annual_fee_bps?",
                     annual_fixed_fee_minor AS "annual_fixed_fee_minor?",
                     notes, created_at, updated_at"#,
        target_type,
        input.target_id,
        input.annual_growth_bps,
        input.annual_volatility_bps,
        input.dividend_yield_bps,
        input.long_run_growth_bps,
        input.notes,
        input.annual_fee_bps,
        input.annual_fixed_fee_minor
    )
    .fetch_one(db)
    .await?
    .try_into()
}

/// Clear the override for `(target_type, target_id)`, if one exists — the target then
/// falls back to a cron-derived or historical default. Not an error if none was set.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn clear_assumption(
    db: &Db,
    target_type: ForecastTargetType,
    target_id: i64,
) -> AppResult<()> {
    let target_type = target_type.as_str();
    sqlx::query!(
        "DELETE FROM forecast_assumptions WHERE target_type=?1 AND target_id=?2",
        target_type,
        target_id
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Sum of `dividends.net_amount_minor` for `account_id` paid on or after `since`
/// (ISO-8601 date) — the numerator for a trailing-window dividend-yield default.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn trailing_dividends_minor(db: &Db, account_id: i64, since: &str) -> AppResult<i64> {
    Ok(sqlx::query_scalar!(
        // SUM over no matching rows is NULL, hence the type override and the `unwrap_or`.
        r#"SELECT SUM(net_amount_minor) AS "total: i64"
             FROM dividends WHERE account_id=?1 AND paid_date >= ?2"#,
        account_id,
        since
    )
    .fetch_one(db)
    .await?
    .unwrap_or(0))
}

// ---- forecast events -------------------------------------------------------------

#[derive(Debug)]
struct ForecastEventRow {
    id: i64,
    label: String,
    kind: String,
    person_id: Option<i64>,
    expected_on: String,
    timing_spread_months: i64,
    probability_bps: i64,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug)]
struct EffectRow {
    id: i64,
    event_id: i64,
    kind: String,
    sort_order: i64,
    income_stream_id: Option<i64>,
    person_id: Option<i64>,
    category_id: Option<i64>,
    account_id: Option<i64>,
    amount_minor: Option<i64>,
    rate_bps: Option<i64>,
    delay_months: Option<i64>,
    ramp_months: Option<i64>,
    duration_months: Option<i64>,
}

impl TryFrom<EffectRow> for ForecastEventEffect {
    type Error = AppError;

    fn try_from(r: EffectRow) -> AppResult<Self> {
        let bad = |e: String| AppError::Internal(anyhow::anyhow!(e));
        let kind: LifeEffectKind = r.kind.parse().map_err(bad)?;
        // `from_columns` refuses every combination the migration's CHECK already makes impossible,
        // so an error here means the row was written around every writer we own. Reported rather
        // than coerced: a silently-defaulted effect is a projection quietly missing the change the
        // user typed.
        let spec = LifeEffectSpec::from_columns(
            kind,
            EffectColumns {
                income_stream_id: r.income_stream_id,
                person_id: r.person_id,
                category_id: r.category_id,
                account_id: r.account_id,
                amount_minor: r.amount_minor,
                rate_bps: r.rate_bps,
                delay_months: r.delay_months,
                ramp_months: r.ramp_months,
                duration_months: r.duration_months,
            },
        )
        .map_err(bad)?;
        Ok(ForecastEventEffect {
            id: r.id,
            event_id: r.event_id,
            sort_order: r.sort_order,
            spec,
        })
    }
}

#[derive(Debug)]
struct RelationRow {
    id: i64,
    event_id: i64,
    depends_on_event_id: i64,
    kind: String,
    min_gap_months: i64,
}

impl TryFrom<RelationRow> for ForecastEventRelation {
    type Error = AppError;

    fn try_from(r: RelationRow) -> AppResult<Self> {
        let kind: RelationKind = r
            .kind
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(ForecastEventRelation {
            id: r.id,
            event_id: r.event_id,
            depends_on_event_id: r.depends_on_event_id,
            kind,
            min_gap_months: r.min_gap_months,
        })
    }
}

/// Every event with its effects and relations attached, soonest first.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_events(db: &Db) -> AppResult<Vec<ForecastEvent>> {
    let rows = sqlx::query_as!(
        ForecastEventRow,
        r#"SELECT id AS "id!", label, kind, person_id, expected_on, timing_spread_months,
                  probability_bps, notes, created_at, updated_at
             FROM forecast_events ORDER BY expected_on, id"#
    )
    .fetch_all(db)
    .await?;
    // Two queries for all the children rather than two per event: this list is always wanted whole.
    let effects = sqlx::query_as!(
        EffectRow,
        r#"SELECT id AS "id!", event_id, kind, sort_order, income_stream_id, person_id,
                  category_id, account_id, amount_minor, rate_bps, delay_months, ramp_months,
                  duration_months
             FROM forecast_event_effects ORDER BY event_id, sort_order, id"#
    )
    .fetch_all(db)
    .await?;
    let relations = sqlx::query_as!(
        RelationRow,
        r#"SELECT id AS "id!", event_id, depends_on_event_id, kind, min_gap_months
             FROM forecast_event_relations ORDER BY event_id, id"#
    )
    .fetch_all(db)
    .await?;

    let mut eff_by: std::collections::HashMap<i64, Vec<ForecastEventEffect>> = Default::default();
    for e in effects {
        let id = e.event_id;
        eff_by.entry(id).or_default().push(e.try_into()?);
    }
    let mut rel_by: std::collections::HashMap<i64, Vec<ForecastEventRelation>> = Default::default();
    for r in relations {
        let id = r.event_id;
        rel_by.entry(id).or_default().push(r.try_into()?);
    }
    rows.into_iter()
        .map(|r| {
            let bad = |e: String| AppError::Internal(anyhow::anyhow!(e));
            Ok(ForecastEvent {
                effects: eff_by.remove(&r.id).unwrap_or_default(),
                relations: rel_by.remove(&r.id).unwrap_or_default(),
                kind: r.kind.parse().map_err(bad)?,
                id: r.id,
                label: r.label,
                person_id: r.person_id,
                expected_on: r.expected_on,
                timing_spread_months: r.timing_spread_months,
                probability_bps: r.probability_bps,
                notes: r.notes,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
        })
        .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get_event(db: &Db, id: i64) -> AppResult<ForecastEvent> {
    list_events(db)
        .await?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or(AppError::NotFound("forecast event"))
}

fn validate(input: &SaveForecastEvent) -> AppResult<()> {
    let mut problems = input.validate().err().unwrap_or_default();
    problems.extend(
        sure_core::effect_amounts_in_range(&input.effects)
            .err()
            .unwrap_or_default(),
    );
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::validation(problems.join("; ")))
    }
}

/// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
/// (CLAUDE.md rule 2's escape hatch).
#[allow(clippy::wildcard_enum_match_arm)]
fn fk_error(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => AppError::validation(
            "an effect points at a person, income stream, account or category that does not exist",
        ),
        sqlx::Error::Database(ref db) if db.is_check_violation() => AppError::validation(
            "an effect is missing a field its kind requires, or carries one it must not",
        ),
        other => AppError::from(other),
    }
}

/// Would the proposed relation set close a cycle?
///
/// Walked at write time so the answer is a sentence naming the loop rather than an opaque failure,
/// and again at resolve time in `sure-app` — the two layers `categories` already uses, where a
/// parent cycle is refused on write but the read path still carries a seen-check so a hand-edited
/// database cannot hang a report.
async fn would_cycle(
    db: &Db,
    event_id: Option<i64>,
    proposed: &[SaveForecastEventRelation],
) -> AppResult<Option<String>> {
    // Every existing edge except this event's own outgoing ones, which the write replaces.
    let mut edges: Vec<(i64, i64)> =
        sqlx::query!("SELECT event_id, depends_on_event_id FROM forecast_event_relations")
            .fetch_all(db)
            .await?
            .into_iter()
            .map(|r| (r.event_id, r.depends_on_event_id))
            .filter(|(from, _)| Some(*from) != event_id)
            .collect();

    // A create has no id yet; a sentinel that cannot collide stands in for it, and any cycle through
    // it is a cycle through the row about to exist.
    let me = event_id.unwrap_or(i64::MIN);
    edges.extend(proposed.iter().map(|r| (me, r.depends_on_event_id)));

    let mut adj: std::collections::HashMap<i64, Vec<i64>> = Default::default();
    for (from, to) in edges {
        adj.entry(from).or_default().push(to);
    }
    // Depth-first from the event being written: if it can reach itself, the set closes a loop.
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![me];
    while let Some(node) = stack.pop() {
        for &next in adj.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
            if next == me {
                return Ok(Some(format!(
                    "that would make this change wait for #{node}, which already waits for it"
                )));
            }
            if seen.insert(next) {
                stack.push(next);
            }
        }
    }
    Ok(None)
}

async fn write_children(
    txn: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event_id: i64,
    input: &SaveForecastEvent,
) -> AppResult<()> {
    sqlx::query!(
        "DELETE FROM forecast_event_effects WHERE event_id=?1",
        event_id
    )
    .execute(&mut **txn)
    .await?;
    sqlx::query!(
        "DELETE FROM forecast_event_relations WHERE event_id=?1",
        event_id
    )
    .execute(&mut **txn)
    .await?;
    for (i, spec) in input.effects.iter().enumerate() {
        let c = spec.as_columns();
        let kind = spec.kind().as_str();
        let sort_order = i as i64;
        sqlx::query!(
            "INSERT INTO forecast_event_effects
                (event_id, kind, sort_order, income_stream_id, person_id, category_id, account_id,
                 amount_minor, rate_bps, delay_months, ramp_months, duration_months)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            event_id,
            kind,
            sort_order,
            c.income_stream_id,
            c.person_id,
            c.category_id,
            c.account_id,
            c.amount_minor,
            c.rate_bps,
            c.delay_months,
            c.ramp_months,
            c.duration_months
        )
        .execute(&mut **txn)
        .await
        .map_err(fk_error)?;
    }
    for r in &input.relations {
        let kind = r.kind.as_str();
        sqlx::query!(
            "INSERT INTO forecast_event_relations
                (event_id, depends_on_event_id, kind, min_gap_months)
             VALUES (?1,?2,?3,?4)",
            event_id,
            r.depends_on_event_id,
            kind,
            r.min_gap_months
        )
        .execute(&mut **txn)
        .await
        .map_err(fk_error)?;
    }
    Ok(())
}

/// Create the event, its effects and its relations in one transaction.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn create_event(db: &Db, input: SaveForecastEvent) -> AppResult<ForecastEvent> {
    validate(&input)?;
    if let Some(cycle) = would_cycle(db, None, &input.relations).await? {
        return Err(AppError::conflict(cycle));
    }
    let mut txn = db.begin().await?;
    let label = input.label.trim();
    let kind = input.kind.as_str();
    let expected_on = input.expected_on.to_string();
    let notes = input.notes.as_deref();
    // Only the id is wanted here: the children go in below and `get_event` re-reads the event
    // whole.
    let id = sqlx::query_scalar!(
        r#"INSERT INTO forecast_events
              (label, kind, person_id, expected_on, timing_spread_months, probability_bps, notes)
           VALUES (?1,?2,?3,?4,?5,?6,?7)
           RETURNING id AS "id!""#,
        label,
        kind,
        input.person_id,
        expected_on,
        input.timing_spread_months,
        input.probability_bps,
        notes
    )
    .fetch_one(&mut *txn)
    .await
    .map_err(fk_error)?;
    write_children(&mut txn, id, &input).await?;
    txn.commit().await?;
    get_event(db, id).await
}

/// Full replace, effects and relations included.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn update_event(db: &Db, id: i64, input: SaveForecastEvent) -> AppResult<ForecastEvent> {
    validate(&input)?;
    if let Some(cycle) = would_cycle(db, Some(id), &input.relations).await? {
        return Err(AppError::conflict(cycle));
    }
    let mut txn = db.begin().await?;
    let label = input.label.trim();
    let kind = input.kind.as_str();
    let expected_on = input.expected_on.to_string();
    let notes = input.notes.as_deref();
    let res = sqlx::query!(
        "UPDATE forecast_events SET
            label=?2, kind=?3, person_id=?4, expected_on=?5, timing_spread_months=?6,
            probability_bps=?7, notes=?8,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1",
        id,
        label,
        kind,
        input.person_id,
        expected_on,
        input.timing_spread_months,
        input.probability_bps,
        notes
    )
    .execute(&mut *txn)
    .await
    .map_err(fk_error)?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("forecast event"));
    }
    write_children(&mut txn, id, &input).await?;
    txn.commit().await?;
    get_event(db, id).await
}

/// Delete an event.
///
/// Asymmetric on purpose, because the two relation kinds differ in what deletion *costs*. A dangling
/// `after` is pure ordering and is dropped silently — refusing would trap the user in a graph they
/// could only escape by editing every dependent first. A dangling `only_if` is refused: an event the
/// user believed was conditional would quietly become unconditional in every future projection,
/// which is a change of meaning with no trace.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete_event(db: &Db, id: i64) -> AppResult<()> {
    let conditional = sqlx::query_scalar!(
        "SELECT e.label
           FROM forecast_event_relations r
           JOIN forecast_events e ON e.id = r.event_id
          WHERE r.depends_on_event_id = ?1 AND r.kind = 'only_if'
          ORDER BY e.label",
        id
    )
    .fetch_all(db)
    .await?;
    if !conditional.is_empty() {
        return Err(AppError::conflict(format!(
            "These changes only happen if this one does, so they would silently become certain: {}",
            crate::people::summarise(&conditional)
        )));
    }
    let mut txn = db.begin().await?;
    // Pure ordering edges pointing at it: dropped, since the ordering is meaningless without it.
    sqlx::query!(
        "DELETE FROM forecast_event_relations WHERE depends_on_event_id=?1",
        id
    )
    .execute(&mut *txn)
    .await?;
    let res = sqlx::query!("DELETE FROM forecast_events WHERE id=?1", id)
        .execute(&mut *txn)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("forecast event"));
    }
    txn.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> Db {
        // A single connection so all queries hit the same in-memory database — a pool
        // with >1 connection would give each connection its own empty :memory: db.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    fn assumption(annual_volatility_bps: Option<i64>) -> SaveForecastAssumption {
        SaveForecastAssumption {
            target_type: ForecastTargetType::Account,
            target_id: 1,
            annual_growth_bps: Some(700),
            annual_volatility_bps,
            dividend_yield_bps: None,
            long_run_growth_bps: None,
            annual_fee_bps: None,
            annual_fixed_fee_minor: None,
            notes: None,
        }
    }

    /// The `GET /api/forecast` permanent-500 guard. A volatility this large makes both an
    /// `exp()` underflow to `0.0` and an overflow to `inf` routine inside one simulated path;
    /// `0.0 * inf` is `NaN`, and a `NaN` used to reach the percentile sort and panic the
    /// request. Refused on the way *in*, so the forecast page can never be taken down by a
    /// row only reachable through a control on that same page.
    #[tokio::test]
    async fn refuses_a_volatility_that_would_overflow_the_simulation() {
        let db = test_db().await;
        let err = upsert_assumption(&db, assumption(Some(10_000_000_000)))
            .await
            .expect_err("an absurd volatility must be refused, not stored");
        assert!(
            matches!(err, AppError::Validation(ref m) if m.contains("annual_volatility_bps")),
            "expected a validation error naming the field, got {err:?}"
        );
        // …and nothing was written, so a retry with a sane value is the whole recovery.
        assert!(list_assumptions(&db).await.unwrap().is_empty());
    }

    /// Negative variance is not a thing. The use-site clamp used to absorb it silently, which
    /// meant a typed minus sign produced a projection with no noise at all rather than a
    /// complaint.
    #[tokio::test]
    async fn refuses_a_negative_volatility() {
        let db = test_db().await;
        assert!(matches!(
            upsert_assumption(&db, assumption(Some(-1))).await,
            Err(AppError::Validation(_))
        ));
    }

    /// The bound is a numerical guard, not an opinion: everything up to it — including the
    /// 300%/yr a genuinely lumpy category really does measure — still stores, and `None`
    /// (derive from history) is untouched by the check.
    #[tokio::test]
    async fn accepts_every_usable_volatility_including_the_ceiling() {
        let db = test_db().await;
        for vol in [None, Some(0), Some(1_500), Some(MAX_VOLATILITY_BPS)] {
            let saved = upsert_assumption(&db, assumption(vol))
                .await
                .unwrap_or_else(|e| panic!("{vol:?} should be accepted: {e:?}"));
            assert_eq!(saved.annual_volatility_bps, vol);
        }
    }

    /// An explicit *growth* override is deliberately unbounded here — that is the user
    /// asserting something about returns, and `sure_app::forecast` clamps it into a safe log
    /// return at the use site. Only volatility, a variance feeding a numerical method, is
    /// refused.
    #[tokio::test]
    async fn leaves_an_explicit_growth_override_alone() {
        let db = test_db().await;
        let saved = upsert_assumption(
            &db,
            SaveForecastAssumption {
                annual_growth_bps: Some(500_000),
                ..assumption(Some(1_000))
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.annual_growth_bps, Some(500_000));
    }
}
