//! Forecast events: what might happen, how likely, roughly when, and what it does.
//!
//! One model for both the certainties the forecast has always had ("my bonus lands in March",
//! now `kind = Adjustment` with 100% probability and no spread) and the probabilistic ones it did
//! not ("a child, some time around 2029, 80% likely"). They were never two different things — the
//! old shape was this one with three fields missing.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::iso_date::IsoDate;
use crate::money::Money;

/// What sort of event this is, for presentation and for choosing a form template.
///
/// **The simulation never branches on this.** It branches on the event's *effects*, which is what
/// makes adding a variant here a UI change rather than a change to the projection. A career break
/// that pauses pay and a job ending that stops it are the same arithmetic; only the words differ.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifeEventKind {
    Promotion,
    Child,
    CareerBreak,
    JobStart,
    JobEnd,
    /// A dated, exact change — what every event was before this model existed.
    Adjustment,
    Custom,
}

impl LifeEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LifeEventKind::Promotion => "promotion",
            LifeEventKind::Child => "child",
            LifeEventKind::CareerBreak => "career_break",
            LifeEventKind::JobStart => "job_start",
            LifeEventKind::JobEnd => "job_end",
            LifeEventKind::Adjustment => "adjustment",
            LifeEventKind::Custom => "custom",
        }
    }
}

impl FromStr for LifeEventKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "promotion" => Ok(LifeEventKind::Promotion),
            "child" => Ok(LifeEventKind::Child),
            "career_break" => Ok(LifeEventKind::CareerBreak),
            "job_start" => Ok(LifeEventKind::JobStart),
            "job_end" => Ok(LifeEventKind::JobEnd),
            "adjustment" => Ok(LifeEventKind::Adjustment),
            "custom" => Ok(LifeEventKind::Custom),
            other => Err(format!("unknown forecast event kind '{other}'")),
        }
    }
}

/// The discriminant of a `forecast_event_effects` row. Text only at the column edge.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifeEffectKind {
    IncomeStep,
    IncomeStart,
    IncomeEnd,
    IncomePause,
    RecurringDelta,
    SetBaseline,
    OneOffAmount,
}

impl LifeEffectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LifeEffectKind::IncomeStep => "income_step",
            LifeEffectKind::IncomeStart => "income_start",
            LifeEffectKind::IncomeEnd => "income_end",
            LifeEffectKind::IncomePause => "income_pause",
            LifeEffectKind::RecurringDelta => "recurring_delta",
            LifeEffectKind::SetBaseline => "set_baseline",
            LifeEffectKind::OneOffAmount => "one_off_amount",
        }
    }
}

impl FromStr for LifeEffectKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "income_step" => Ok(LifeEffectKind::IncomeStep),
            "income_start" => Ok(LifeEffectKind::IncomeStart),
            "income_end" => Ok(LifeEffectKind::IncomeEnd),
            "income_pause" => Ok(LifeEffectKind::IncomePause),
            "recurring_delta" => Ok(LifeEffectKind::RecurringDelta),
            "set_baseline" => Ok(LifeEffectKind::SetBaseline),
            "one_off_amount" => Ok(LifeEffectKind::OneOffAmount),
            other => Err(format!("unknown forecast effect kind '{other}'")),
        }
    }
}

/// How a promotion moves a stream's level.
///
/// Absolute versus relative matters: "+12%" composes with an earlier promotion and with the dated
/// pay scale underneath it, and an absolute figure does not.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum StepAmount {
    Absolute { annual_amount_minor: i64 },
    Percent { rate_bps: i64 },
}

/// What a baseline change or a one-off lands on.
///
/// An account amount is in that account's own currency; a category amount is in the base reporting
/// currency, because a category has no currency of its own. That asymmetry is inherited from how
/// the projection already treats the two and is why this is a tagged union rather than a pair of
/// nullable ids.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectTarget {
    Account { account_id: i64 },
    Category { category_id: i64 },
}

/// One thing an event does. An event has N of them, which is what lets "a child" mean daycare *and*
/// a paused salary *and* a pram in one place.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifeEffectSpec {
    /// A promotion.
    IncomeStep {
        income_stream_id: i64,
        amount: StepAmount,
    },
    /// A job begins: the stream pays from the sampled month, overriding its own `starts_on`.
    IncomeStart { income_stream_id: i64 },
    /// A job ends: the stream stops paying from the sampled month.
    IncomeEnd { income_stream_id: i64 },
    /// A career break. Pauses **every** stream this person has for `months`, paying
    /// `replacement_rate_bps` of normal meanwhile (0 = unpaid, 10000 = fully paid leave).
    ///
    /// Per person rather than per stream because that is what a career break is — nobody takes
    /// parental leave from one of their two jobs.
    IncomePause {
        person_id: i64,
        months: i64,
        replacement_rate_bps: i64,
    },
    /// An ongoing cost that starts `delay_months` after the event, ramps linearly to full over
    /// `ramp_months` (0 = arrives at full cost), and optionally stops after `duration_months`.
    ///
    /// Daycare does not begin the day a child is born and school fees do not end, so both ends are
    /// expressible. **Additive** on top of the category's fitted baseline, unlike `SetBaseline` —
    /// that distinction is the reason both exist.
    RecurringDelta {
        category_id: i64,
        amount_minor: i64,
        delay_months: i64,
        ramp_months: i64,
        duration_months: Option<i64>,
    },
    /// Replace an account's value or a category's ongoing monthly baseline from this month on.
    SetBaseline {
        target: EffectTarget,
        amount_minor: i64,
    },
    /// A single-month delta: a bonus, a pram, moving costs.
    OneOffAmount {
        target: EffectTarget,
        amount_minor: i64,
    },
}

/// The nine nullable columns a `forecast_event_effects` row carries.
///
/// This type exists so the columns↔union mapping is one function each way rather than a pair of
/// hand-copied matches in the DAL — the same reason `Ownership::as_parts` exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectColumns {
    pub income_stream_id: Option<i64>,
    pub person_id: Option<i64>,
    pub category_id: Option<i64>,
    pub account_id: Option<i64>,
    pub amount_minor: Option<i64>,
    pub rate_bps: Option<i64>,
    pub delay_months: Option<i64>,
    pub ramp_months: Option<i64>,
    pub duration_months: Option<i64>,
}

impl LifeEffectSpec {
    pub fn kind(self) -> LifeEffectKind {
        match self {
            LifeEffectSpec::IncomeStep { .. } => LifeEffectKind::IncomeStep,
            LifeEffectSpec::IncomeStart { .. } => LifeEffectKind::IncomeStart,
            LifeEffectSpec::IncomeEnd { .. } => LifeEffectKind::IncomeEnd,
            LifeEffectSpec::IncomePause { .. } => LifeEffectKind::IncomePause,
            LifeEffectSpec::RecurringDelta { .. } => LifeEffectKind::RecurringDelta,
            LifeEffectSpec::SetBaseline { .. } => LifeEffectKind::SetBaseline,
            LifeEffectSpec::OneOffAmount { .. } => LifeEffectKind::OneOffAmount,
        }
    }

    /// The columns to bind. The only place these values become columns.
    pub fn as_columns(self) -> EffectColumns {
        let target_cols = |t: EffectTarget| match t {
            EffectTarget::Account { account_id } => (Some(account_id), None),
            EffectTarget::Category { category_id } => (None, Some(category_id)),
        };
        match self {
            LifeEffectSpec::IncomeStep {
                income_stream_id,
                amount,
            } => {
                let (amount_minor, rate_bps) = match amount {
                    StepAmount::Absolute {
                        annual_amount_minor,
                    } => (Some(annual_amount_minor), None),
                    StepAmount::Percent { rate_bps } => (None, Some(rate_bps)),
                };
                EffectColumns {
                    income_stream_id: Some(income_stream_id),
                    amount_minor,
                    rate_bps,
                    ..EffectColumns::default()
                }
            }
            LifeEffectSpec::IncomeStart { income_stream_id }
            | LifeEffectSpec::IncomeEnd { income_stream_id } => EffectColumns {
                income_stream_id: Some(income_stream_id),
                ..EffectColumns::default()
            },
            LifeEffectSpec::IncomePause {
                person_id,
                months,
                replacement_rate_bps,
            } => EffectColumns {
                person_id: Some(person_id),
                duration_months: Some(months),
                rate_bps: Some(replacement_rate_bps),
                ..EffectColumns::default()
            },
            LifeEffectSpec::RecurringDelta {
                category_id,
                amount_minor,
                delay_months,
                ramp_months,
                duration_months,
            } => EffectColumns {
                category_id: Some(category_id),
                amount_minor: Some(amount_minor),
                delay_months: Some(delay_months),
                ramp_months: Some(ramp_months),
                duration_months,
                ..EffectColumns::default()
            },
            LifeEffectSpec::SetBaseline {
                target,
                amount_minor,
            }
            | LifeEffectSpec::OneOffAmount {
                target,
                amount_minor,
            } => {
                let (account_id, category_id) = target_cols(target);
                EffectColumns {
                    account_id,
                    category_id,
                    amount_minor: Some(amount_minor),
                    ..EffectColumns::default()
                }
            }
        }
    }

    /// Rebuild from the stored columns.
    ///
    /// Every combination this refuses is one the migration's `CHECK` already makes impossible, so an
    /// `Err` means the row was written by something that went around every writer we own. Reported
    /// as such — exactly like `Ownership::from_stored` — rather than coerced into whichever variant
    /// looks closest, because a silently-defaulted effect is a projection quietly missing the
    /// promotion the user typed.
    pub fn from_columns(kind: LifeEffectKind, c: EffectColumns) -> Result<Self, String> {
        let want = |v: Option<i64>, field: &str| {
            v.ok_or_else(|| format!("{} effect is missing {field}", kind.as_str()))
        };
        let target = |c: &EffectColumns| match (c.account_id, c.category_id) {
            (Some(account_id), None) => Ok(EffectTarget::Account { account_id }),
            (None, Some(category_id)) => Ok(EffectTarget::Category { category_id }),
            (Some(_), Some(_)) => Err(format!(
                "{} effect targets both an account and a category",
                kind.as_str()
            )),
            (None, None) => Err(format!("{} effect targets nothing", kind.as_str())),
        };
        match kind {
            LifeEffectKind::IncomeStep => {
                let income_stream_id = want(c.income_stream_id, "income_stream_id")?;
                let amount = match (c.amount_minor, c.rate_bps) {
                    (Some(annual_amount_minor), None) => StepAmount::Absolute {
                        annual_amount_minor,
                    },
                    (None, Some(rate_bps)) => StepAmount::Percent { rate_bps },
                    (Some(_), Some(_)) => {
                        return Err(
                            "income_step has both an absolute amount and a percentage".into()
                        )
                    }
                    (None, None) => {
                        return Err("income_step has neither an amount nor a rate".into())
                    }
                };
                Ok(LifeEffectSpec::IncomeStep {
                    income_stream_id,
                    amount,
                })
            }
            LifeEffectKind::IncomeStart => Ok(LifeEffectSpec::IncomeStart {
                income_stream_id: want(c.income_stream_id, "income_stream_id")?,
            }),
            LifeEffectKind::IncomeEnd => Ok(LifeEffectSpec::IncomeEnd {
                income_stream_id: want(c.income_stream_id, "income_stream_id")?,
            }),
            LifeEffectKind::IncomePause => Ok(LifeEffectSpec::IncomePause {
                person_id: want(c.person_id, "person_id")?,
                months: want(c.duration_months, "duration_months")?,
                replacement_rate_bps: want(c.rate_bps, "rate_bps")?,
            }),
            LifeEffectKind::RecurringDelta => Ok(LifeEffectSpec::RecurringDelta {
                category_id: want(c.category_id, "category_id")?,
                amount_minor: want(c.amount_minor, "amount_minor")?,
                delay_months: want(c.delay_months, "delay_months")?,
                ramp_months: want(c.ramp_months, "ramp_months")?,
                duration_months: c.duration_months,
            }),
            LifeEffectKind::SetBaseline => Ok(LifeEffectSpec::SetBaseline {
                target: target(&c)?,
                amount_minor: want(c.amount_minor, "amount_minor")?,
            }),
            LifeEffectKind::OneOffAmount => Ok(LifeEffectSpec::OneOffAmount {
                target: target(&c)?,
                amount_minor: want(c.amount_minor, "amount_minor")?,
            }),
        }
    }
}

/// How one event constrains another.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// At least `min_gap_months` after the parent's sampled month, applied by clamping up.
    After,
    /// Only on paths where the parent occurred.
    OnlyIf,
}

impl RelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RelationKind::After => "after",
            RelationKind::OnlyIf => "only_if",
        }
    }
}

impl FromStr for RelationKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "after" => Ok(RelationKind::After),
            "only_if" => Ok(RelationKind::OnlyIf),
            other => Err(format!("unknown forecast event relation kind '{other}'")),
        }
    }
}

#[derive(Debug, Serialize, ToSchema, Clone, Copy)]
pub struct ForecastEventRelation {
    pub id: i64,
    pub event_id: i64,
    pub depends_on_event_id: i64,
    pub kind: RelationKind,
    pub min_gap_months: i64,
}

#[derive(Debug, Deserialize, ToSchema, Clone, Copy)]
pub struct SaveForecastEventRelation {
    pub depends_on_event_id: i64,
    pub kind: RelationKind,
    #[serde(default)]
    pub min_gap_months: i64,
}

#[derive(Debug, Serialize, ToSchema, Clone, Copy)]
pub struct ForecastEventEffect {
    pub id: i64,
    pub event_id: i64,
    pub sort_order: i64,
    #[serde(flatten)]
    pub spec: LifeEffectSpec,
}

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct ForecastEvent {
    pub id: i64,
    pub label: String,
    pub kind: LifeEventKind,
    pub person_id: Option<i64>,
    pub expected_on: String,
    /// Half-width of a uniform hard window, in months. 0 = the date is certain.
    pub timing_spread_months: i64,
    pub probability_bps: i64,
    pub notes: Option<String>,
    pub effects: Vec<ForecastEventEffect>,
    pub relations: Vec<ForecastEventRelation>,
    pub created_at: String,
    pub updated_at: String,
}

/// Write body: a **full replace**, effects and relations included.
///
/// One body and one transaction, for three reasons. A partial save (event stored, effect rejected)
/// leaves a state the user cannot see and did not ask for; every problem across every effect can be
/// collected into one 422, which is the `AccountMetadata::validate_for` contract; and the cycle check
/// needs the *complete proposed graph*, which a per-relation endpoint could only ever validate
/// mid-edit.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveForecastEvent {
    pub label: String,
    pub kind: LifeEventKind,
    #[serde(default)]
    pub person_id: Option<i64>,
    #[schema(value_type = String)]
    pub expected_on: IsoDate,
    #[serde(default)]
    pub timing_spread_months: i64,
    #[serde(default = "certain")]
    pub probability_bps: i64,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub effects: Vec<LifeEffectSpec>,
    #[serde(default)]
    pub relations: Vec<SaveForecastEventRelation>,
}

/// A body that says nothing about probability is asserting a certainty — which is what every event
/// in this table meant before probability existed.
fn certain() -> i64 {
    10_000
}

impl SaveForecastEvent {
    /// Every problem at once, not the first one found.
    ///
    /// Ranges only: serde already refuses an unknown `kind` or a missing tagged field, and the
    /// migration's `CHECK` refuses a malformed column combination. This is the layer that turns
    /// "constraint failed" into a sentence naming the field.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        if self.label.trim().is_empty() {
            problems.push("label must not be empty".to_string());
        }
        if !(0..=10_000).contains(&self.probability_bps) {
            problems.push(format!(
                "probability_bps must be between 0 and 10000, got {}",
                self.probability_bps
            ));
        }
        if !(0..=600).contains(&self.timing_spread_months) {
            problems.push(format!(
                "timing_spread_months must be between 0 and 600, got {}",
                self.timing_spread_months
            ));
        }
        for (i, e) in self.effects.iter().enumerate() {
            let at = |m: String| format!("effect {i}: {m}");
            match *e {
                LifeEffectSpec::IncomePause {
                    months,
                    replacement_rate_bps,
                    ..
                } => {
                    if months <= 0 {
                        problems.push(at("a pause must last at least one month".into()));
                    }
                    if !(0..=10_000).contains(&replacement_rate_bps) {
                        problems.push(at(format!(
                            "replacement_rate_bps must be between 0 and 10000, got {replacement_rate_bps}"
                        )));
                    }
                }
                LifeEffectSpec::RecurringDelta {
                    delay_months,
                    ramp_months,
                    duration_months,
                    ..
                } => {
                    if delay_months < 0 || ramp_months < 0 {
                        problems.push(at("delay and ramp cannot be negative".into()));
                    }
                    if duration_months.is_some_and(|d| d <= 0) {
                        problems.push(at("a duration must be at least one month".into()));
                    }
                }
                LifeEffectSpec::IncomeStep { amount, .. } => {
                    if let StepAmount::Percent { rate_bps } = amount {
                        // -100% is "stops paying", which is what `IncomeEnd` is for; below that is
                        // a negative salary.
                        if rate_bps <= -10_000 {
                            problems.push(at(
                                "a pay change below -100% is not a pay change — use a job ending"
                                    .into(),
                            ));
                        }
                    }
                }
                LifeEffectSpec::IncomeStart { .. }
                | LifeEffectSpec::IncomeEnd { .. }
                | LifeEffectSpec::SetBaseline { .. }
                | LifeEffectSpec::OneOffAmount { .. } => {}
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

/// `Money`'s bound, applied to the amounts inside the effect union.
///
/// The union's amounts are plain `i64` rather than `Money`, because `#[serde(tag)]` on a nested enum
/// and a validating newtype do not compose cleanly — so the bound is applied here instead, at the
/// same edge, rather than not at all.
pub fn effect_amounts_in_range(effects: &[LifeEffectSpec]) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    let check = |v: i64, problems: &mut Vec<String>| {
        if Money::new(v).is_err() {
            problems.push(format!("amount {v} is out of range"));
        }
    };
    for e in effects {
        match *e {
            LifeEffectSpec::IncomeStep { amount, .. } => {
                if let StepAmount::Absolute {
                    annual_amount_minor,
                } = amount
                {
                    check(annual_amount_minor, &mut problems);
                }
            }
            LifeEffectSpec::RecurringDelta { amount_minor, .. }
            | LifeEffectSpec::SetBaseline { amount_minor, .. }
            | LifeEffectSpec::OneOffAmount { amount_minor, .. } => {
                check(amount_minor, &mut problems)
            }
            LifeEffectSpec::IncomeStart { .. }
            | LifeEffectSpec::IncomeEnd { .. }
            | LifeEffectSpec::IncomePause { .. } => {}
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_spec() -> Vec<LifeEffectSpec> {
        vec![
            LifeEffectSpec::IncomeStep {
                income_stream_id: 7,
                amount: StepAmount::Absolute {
                    annual_amount_minor: 96_000_00,
                },
            },
            LifeEffectSpec::IncomeStep {
                income_stream_id: 7,
                amount: StepAmount::Percent { rate_bps: 1_200 },
            },
            LifeEffectSpec::IncomeStart {
                income_stream_id: 8,
            },
            LifeEffectSpec::IncomeEnd {
                income_stream_id: 8,
            },
            LifeEffectSpec::IncomePause {
                person_id: 3,
                months: 12,
                replacement_rate_bps: 6_000,
            },
            LifeEffectSpec::RecurringDelta {
                category_id: 11,
                amount_minor: 1_200_00,
                delay_months: 12,
                ramp_months: 3,
                duration_months: Some(48),
            },
            LifeEffectSpec::RecurringDelta {
                category_id: 11,
                amount_minor: 400_00,
                delay_months: 0,
                ramp_months: 0,
                duration_months: None,
            },
            LifeEffectSpec::SetBaseline {
                target: EffectTarget::Category { category_id: 4 },
                amount_minor: 5_000_00,
            },
            LifeEffectSpec::SetBaseline {
                target: EffectTarget::Account { account_id: 2 },
                amount_minor: 770_000_00,
            },
            LifeEffectSpec::OneOffAmount {
                target: EffectTarget::Account { account_id: 2 },
                amount_minor: -3_000_00,
            },
            LifeEffectSpec::OneOffAmount {
                target: EffectTarget::Category { category_id: 4 },
                amount_minor: 900_00,
            },
        ]
    }

    /// The columns↔union seam, both ways, for every variant. If this holds, no effect can be stored
    /// and read back as a different thing — which is the whole reason the mapping is one function
    /// each way rather than two hand-written matches in the DAL.
    #[test]
    fn every_effect_round_trips_through_its_columns() {
        for spec in every_spec() {
            let cols = spec.as_columns();
            let back = LifeEffectSpec::from_columns(spec.kind(), cols)
                .unwrap_or_else(|e| panic!("{spec:?} failed to round-trip: {e}"));
            assert_eq!(spec, back);
        }
    }

    /// And every combination that should be impossible is an error naming the row, never a value
    /// coerced into whichever variant looks closest.
    #[test]
    fn an_impossible_column_combination_is_an_error_not_a_coercion() {
        // A step with neither an amount nor a rate.
        assert!(LifeEffectSpec::from_columns(
            LifeEffectKind::IncomeStep,
            EffectColumns {
                income_stream_id: Some(1),
                ..EffectColumns::default()
            }
        )
        .is_err());
        // …and one with both.
        assert!(LifeEffectSpec::from_columns(
            LifeEffectKind::IncomeStep,
            EffectColumns {
                income_stream_id: Some(1),
                amount_minor: Some(1),
                rate_bps: Some(1),
                ..EffectColumns::default()
            }
        )
        .is_err());
        // A one-off pointing at nothing, and at two things.
        assert!(LifeEffectSpec::from_columns(
            LifeEffectKind::OneOffAmount,
            EffectColumns {
                amount_minor: Some(1),
                ..EffectColumns::default()
            }
        )
        .is_err());
        assert!(LifeEffectSpec::from_columns(
            LifeEffectKind::OneOffAmount,
            EffectColumns {
                amount_minor: Some(1),
                account_id: Some(1),
                category_id: Some(2),
                ..EffectColumns::default()
            }
        )
        .is_err());
        // A pause with no duration.
        assert!(LifeEffectSpec::from_columns(
            LifeEffectKind::IncomePause,
            EffectColumns {
                person_id: Some(1),
                rate_bps: Some(0),
                ..EffectColumns::default()
            }
        )
        .is_err());
        // Every error names the kind, so a corrupt row can be found.
        let e = LifeEffectSpec::from_columns(LifeEffectKind::IncomePause, EffectColumns::default())
            .unwrap_err();
        assert!(e.contains("income_pause"), "{e}");
    }

    #[test]
    fn every_enum_round_trips_through_its_stored_text() {
        for k in [
            LifeEventKind::Promotion,
            LifeEventKind::Child,
            LifeEventKind::CareerBreak,
            LifeEventKind::JobStart,
            LifeEventKind::JobEnd,
            LifeEventKind::Adjustment,
            LifeEventKind::Custom,
        ] {
            assert_eq!(LifeEventKind::from_str(k.as_str()), Ok(k));
        }
        for k in [
            LifeEffectKind::IncomeStep,
            LifeEffectKind::IncomeStart,
            LifeEffectKind::IncomeEnd,
            LifeEffectKind::IncomePause,
            LifeEffectKind::RecurringDelta,
            LifeEffectKind::SetBaseline,
            LifeEffectKind::OneOffAmount,
        ] {
            assert_eq!(LifeEffectKind::from_str(k.as_str()), Ok(k));
        }
        for k in [RelationKind::After, RelationKind::OnlyIf] {
            assert_eq!(RelationKind::from_str(k.as_str()), Ok(k));
        }
        assert!(LifeEventKind::from_str("sabbatical").is_err());
    }

    fn save(probability_bps: i64, spread: i64, effects: Vec<LifeEffectSpec>) -> SaveForecastEvent {
        SaveForecastEvent {
            label: "First child".into(),
            kind: LifeEventKind::Child,
            person_id: None,
            expected_on: IsoDate::parse("2029-06-01").unwrap(),
            timing_spread_months: spread,
            probability_bps,
            notes: None,
            effects,
            relations: vec![],
        }
    }

    #[test]
    fn validation_collects_every_problem_at_once() {
        let mut body = save(
            20_000,
            9_999,
            vec![LifeEffectSpec::IncomePause {
                person_id: 1,
                months: 0,
                replacement_rate_bps: 40_000,
            }],
        );
        body.label = "  ".into();
        let problems = body.validate().unwrap_err();
        let all = problems.join("; ");
        assert!(all.contains("label"), "{all}");
        assert!(all.contains("probability_bps"), "{all}");
        assert!(all.contains("timing_spread_months"), "{all}");
        assert!(all.contains("at least one month"), "{all}");
        assert!(all.contains("replacement_rate_bps"), "{all}");
    }

    #[test]
    fn the_boundaries_of_probability_and_spread_are_allowed() {
        // 0% is "modelled but off" and 100% is a certainty. Both are meaningful, so both are legal.
        assert!(save(0, 0, vec![]).validate().is_ok());
        assert!(save(10_000, 600, vec![]).validate().is_ok());
    }

    /// An event with no effects changes nothing and is still legal — a placeholder someone is part
    /// way through filling in should not be refused.
    #[test]
    fn an_event_with_no_effects_is_legal() {
        assert!(save(8_000, 24, vec![]).validate().is_ok());
    }

    #[test]
    fn a_pay_cut_past_minus_one_hundred_percent_is_refused() {
        let body = save(
            10_000,
            0,
            vec![LifeEffectSpec::IncomeStep {
                income_stream_id: 1,
                amount: StepAmount::Percent { rate_bps: -12_000 },
            }],
        );
        assert!(body.validate().is_err());
    }

    #[test]
    fn an_out_of_range_amount_is_refused_at_the_edge() {
        assert!(effect_amounts_in_range(&every_spec()).is_ok());
        assert!(effect_amounts_in_range(&[LifeEffectSpec::OneOffAmount {
            target: EffectTarget::Account { account_id: 1 },
            amount_minor: i64::MAX,
        }])
        .is_err());
    }
}
