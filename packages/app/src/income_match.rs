//! Matching expected pays against the deposits that satisfied them.
//!
//! The other half of `crate::income`: that module says when a stream *should* pay and what a
//! payslip's arithmetic is; this one walks the schedule against the ledger and materialises the
//! result as `income_payments` rows — expected, then matched, each match carrying the
//! decomposition reconstructed from the observed net (`sure_core::tax::reconstruct_period`,
//! because the bank deposit is ground truth and the lines must reconcile to it exactly).
//!
//! Shaped like `import::routing`'s history matcher, for the same reasons its constants carry:
//! candidate windows lean *early* (payroll shifts off weekends and holidays, so a deposit lands
//! before its scheduled date far more often than after), assignment is greedy one-to-one so one
//! deposit cannot satisfy two paydays, and every run regenerates the expected schedule from the
//! current config so an edited stream never strands phantom "missed" pays.
//!
//! A `take_home_bps` override is deliberately ignored here: it steers the *projection*'s
//! gross→net rate, but a matched deposit is observed reality and the statutory inverse is the
//! only itemisation on offer — an override cannot say how much of the wedge was ACC.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{Duration, NaiveDate};
use sure_core::{
    AppError, AppResult, ExtraPayInput, IncomeBasis, IncomePayment, IncomePaymentStatus,
    IncomeStream, MatchedBy, PayTreatment, PayeBreakdown, PeriodPayeInput, Transaction, tax,
};

use crate::income::{TaxScales, payment_dates};
use crate::ports::{Clock, IncomeRepo};

/// How many days *before* its scheduled date a deposit may land and still match. Three, not
/// `detect.rs`'s two: a payday on a Monday after a long weekend pays the previous Friday.
const DAYS_EARLY: i64 = 3;
/// And how many after — payroll almost never runs late, so this stays tight to keep the window
/// clear of the next payday.
const DAYS_LATE: i64 = 1;
/// The schedule is enumerated this far past today, so the row for a payday later this week
/// already exists when its (early) deposit syncs.
const HORIZON_DAYS: i64 = 3;
/// An auto-match must land within `max($5, 2%)` of the predicted net: wide enough for rounding
/// and a small allowance nobody modelled, too narrow for someone else's salary.
const TOLERANCE_FLOOR_MINOR: i64 = 5_00;
const TOLERANCE_BPS: i64 = 200;
/// A bonus slice implied by `observed − base` may exceed its configured level by this much
/// before the deposit is left for review instead of auto-matched — bonuses genuinely vary.
const EXTRA_HEADROOM_BPS: i64 = 1_500;

/// What one run did, for the task's log line.
#[derive(Debug, Default, Clone, Copy)]
pub struct MatchRunSummary {
    /// Matched/confirmed rows whose transaction had been deleted, reset to expected.
    pub repaired: u64,
    /// Expected rows upserted (schedule regeneration touches every unsettled date).
    pub generated: usize,
    /// Stale expected rows removed after a schedule edit.
    pub pruned: usize,
    /// Payments matched to a deposit this run.
    pub matched: usize,
    /// Already-settled payments whose stored payslip no longer followed the stream's terms.
    pub redecomposed: usize,
}

pub struct IncomeMatchService {
    income: Arc<dyn IncomeRepo>,
    clock: Arc<dyn Clock>,
}

impl IncomeMatchService {
    pub fn new(income: Arc<dyn IncomeRepo>, clock: Arc<dyn Clock>) -> Self {
        Self { income, clock }
    }

    /// Salaries the ledger appears to contain, for someone about to record one by hand.
    ///
    /// Reads a two-year window, which is enough to see an annual payment twice and far more than
    /// enough for anything more frequent.
    ///
    /// Lives here rather than on a service of its own because it needs exactly what this one
    /// already holds — the income repo and a clock. It hung off `ForecastService` until the
    /// forecast engine was removed, which was never where it belonged: detecting a salary in the
    /// ledger is a statement about what *has* happened, not a projection of what will.
    pub async fn detect_income(
        &self,
        account_id: Option<i64>,
    ) -> AppResult<Vec<crate::detect::DetectedStream>> {
        let today = self.clock.today();
        let from = (today - chrono::Duration::days(730)).to_string();
        let txns = self.income.income_transactions(&from, account_id).await?;
        Ok(crate::detect::detect(&txns, today))
    }

    /// One full pass: repair orphans, regenerate every matchable stream's expected schedule,
    /// and claim whatever deposits the ledger now holds. Idempotent — every write is either an
    /// upsert behind a unique key or guarded on status — so the scheduler can run it forever.
    pub async fn run(&self) -> AppResult<MatchRunSummary> {
        let mut summary = MatchRunSummary {
            repaired: self.income.reset_orphaned_payments().await?,
            ..Default::default()
        };

        let streams = self.income.list_income_streams().await?;
        let matchable: Vec<&IncomeStream> = streams
            .iter()
            .filter(|s| s.enabled && !s.match_targets.is_empty())
            .collect();
        if matchable.is_empty() {
            return Ok(summary);
        }
        // The two kinds are matched by different means and must not be mixed: a scheduled stream
        // is reconciled against paydays it enumerated, a variable one has no paydays at all.
        let (scheduled, variable): (Vec<&IncomeStream>, Vec<&IncomeStream>) =
            matchable.iter().partition(|s| s.pay_pattern.has_schedule());
        let scales = TaxScales::new(&self.income.list_tax_scales().await?);
        let today = self.clock.today();

        // ---- regenerate the expected schedule --------------------------------------
        let mut expected: HashMap<i64, BTreeMap<NaiveDate, i64>> = HashMap::new();
        for s in &scheduled {
            let Some(anchor) = crate::reports::parse_date_pub(&s.first_payment_on) else {
                continue; // an unparseable anchor has no schedule to regenerate
            };
            let from = crate::reports::parse_date_pub(&s.starts_on)
                .map(|d| d.max(anchor))
                .unwrap_or(anchor);
            // A stream that has ended stops being paid on the day it ended. Without this the
            // schedule ran to the horizon regardless and every date after the end became a
            // "missed pay" that no deposit could ever satisfy — a finished job reporting itself
            // as unpaid, forever, and growing by one row per payday.
            let horizon = match crate::reports::parse_date_pub(s.ends_on.as_deref().unwrap_or("")) {
                Some(end) => end.min(today + Duration::days(HORIZON_DAYS)),
                None => today + Duration::days(HORIZON_DAYS),
            };
            let mut dates = BTreeMap::new();
            for due in payment_dates(s.pay_frequency, anchor, from, horizon) {
                let net = expected_net(
                    s,
                    due,
                    &scales,
                    person_regular_annualised_on(&streams, s.ownership.person_id(), due),
                );
                self.income
                    .upsert_expected_payment(s.id, &due.to_string(), net)
                    .await?;
                summary.generated += 1;
                dates.insert(due, net);
            }
            // Prune strays: expected rows on dates the (possibly edited) schedule no longer
            // contains. Settled rows are never touched — the DAL guards on status too.
            for stale in self.income.expected_payment_due_ons(s.id).await? {
                if crate::reports::parse_date_pub(&stale).is_none_or(|d| !dates.contains_key(&d)) {
                    self.income.delete_expected_payment(s.id, &stale).await?;
                    summary.pruned += 1;
                }
            }
            expected.insert(s.id, dates);
        }

        // ---- claim deposits --------------------------------------------------------
        let mut claimed: HashSet<i64> = self
            .income
            .claimed_transaction_ids()
            .await?
            .into_iter()
            .collect();
        let settled = self
            .income
            .list_income_payments(None, None, None, None)
            .await?;
        // The most recent observed net per stream, seeding the base-slice rule for deposits
        // that carry a bonus on top of the salary (see `split_base_and_extra`).
        let mut last_observed = latest_observed_by_stream(settled.clone());

        summary.redecomposed = self
            .redecompose_settled(&settled, &streams, &scales)
            .await?;

        // Streams sharing a target coalesce: their same-day expected pays are one deposit. A
        // stream with several targets appears in several groups, which is the point — the groups
        // are searched in turn and `claimed` is shared, so one deposit still satisfies one
        // payday, and each group re-reads which of the stream's dates are still open.
        let mut groups: BTreeMap<(i64, String), Vec<&IncomeStream>> = BTreeMap::new();
        for s in &scheduled {
            for key in match_keys(s) {
                groups.entry(key).or_default().push(s);
            }
        }

        for ((account_id, pattern), members) in groups {
            // Which dates still need a deposit — only rows still `expected` are claimable.
            let mut open: BTreeMap<NaiveDate, Vec<&IncomeStream>> = BTreeMap::new();
            for s in &members {
                for due in self.income.expected_payment_due_ons(s.id).await? {
                    if let Some(d) = crate::reports::parse_date_pub(&due) {
                        open.entry(d).or_default().push(s);
                    }
                }
            }
            let Some(earliest) = open.keys().next().copied() else {
                continue;
            };

            let from = (earliest - Duration::days(DAYS_EARLY)).to_string();
            let mut candidates: Vec<Transaction> = self
                .income
                .income_transactions(&from, Some(account_id))
                .await?
                .into_iter()
                .filter(|t| {
                    t.amount_minor > 0
                        // A transfer leg is internal movement, whatever its memo says — the
                        // same guard `detect.rs` applies.
                        && t.linked_transaction_id.is_none()
                        && !claimed.contains(&t.id)
                        && t.description.to_lowercase().contains(&pattern)
                })
                .collect();
            candidates.sort_by(|a, b| a.posted_at.cmp(&b.posted_at));

            for (due, contributors) in open {
                let predicted: i64 = contributors
                    .iter()
                    .filter_map(|s| expected.get(&s.id).and_then(|d| d.get(&due)))
                    .sum();
                let Some(best) = best_candidate(&candidates, &claimed, due, predicted) else {
                    continue;
                };
                let observed = best.amount_minor;
                let tx_id = best.id;
                let tx_currency = best.currency_code.clone();

                let Some(slices) = plan_slices(
                    &contributors,
                    &expected,
                    &last_observed,
                    due,
                    observed,
                    &tx_currency,
                ) else {
                    continue; // needs a person: negative bonus residual, odd shape, wrong currency
                };
                for (stream, slice) in slices {
                    let breakdown = decompose(
                        stream,
                        due,
                        slice,
                        &scales,
                        person_regular_annualised_on(&streams, stream.ownership.person_id(), due),
                    );
                    self.income
                        .record_payment_match(
                            stream.id,
                            &due.to_string(),
                            tx_id,
                            MatchedBy::Auto,
                            IncomePaymentStatus::Matched,
                            slice,
                            &breakdown,
                        )
                        .await?;
                    if stream.pay_treatment == PayTreatment::Regular {
                        last_observed.insert(stream.id, slice);
                    }
                    summary.matched += 1;
                }
                claimed.insert(tx_id);
            }
        }

        // ---- claim deposits for variable streams -----------------------------------
        //
        // **After** the scheduled pass, deliberately. A scheduled stream knows what it expects
        // and when, so it is the more specific claim on a shared deposit; a variable stream takes
        // whatever its memo matches. Running variable first would let a broad pattern — the kind
        // that also catches an employer's expense reimbursements — swallow a salary that had a
        // payday waiting for it. `claimed` is shared, so the ordering is the whole guard.
        for s in &variable {
            summary.matched += self
                .claim_variable(s, &streams, &scales, &mut claimed, today)
                .await?;
        }
        Ok(summary)
    }

    /// Every unclaimed deposit a variable stream's targets match, turned into a payment.
    ///
    /// There is no window and no tolerance here, because there is nothing to have a window or a
    /// tolerance *around*: the stream does not claim to know when it is paid or how much. What
    /// bounds it instead is the three things it does know — which account, what the memo says,
    /// and between which dates it ran. The payment is dated the day the money landed, and
    /// `expected_net_minor` stays NULL, which is what stops the review UI drawing a drift against
    /// a figure nobody predicted.
    async fn claim_variable(
        &self,
        stream: &IncomeStream,
        streams: &[IncomeStream],
        scales: &TaxScales,
        claimed: &mut HashSet<i64>,
        today: NaiveDate,
    ) -> AppResult<usize> {
        let from = crate::reports::parse_date_pub(&stream.starts_on);
        let until = crate::reports::parse_date_pub(stream.ends_on.as_deref().unwrap_or(""))
            .unwrap_or(today)
            .min(today);
        // A stream switched from scheduled to variable leaves its enumerated paydays behind, and
        // nothing else would ever clear them: the schedule pass no longer visits this stream, so
        // its pruning cannot run. They are phantoms — a date nothing will ever satisfy — so they
        // go. Settled rows are untouched; the DAL guards the delete on status.
        for stale in self.income.expected_payment_due_ons(stream.id).await? {
            self.income
                .delete_expected_payment(stream.id, &stale)
                .await?;
        }

        let mut matched = 0;
        for (account_id, pattern) in match_keys(stream) {
            let search_from = from.map(|d| d.to_string()).unwrap_or_default();
            let deposits = self
                .income
                .income_transactions(&search_from, Some(account_id))
                .await?;
            for t in deposits {
                let Some(posted) = crate::reports::parse_date_pub(&t.posted_at) else {
                    continue;
                };
                if from.is_some_and(|f| posted < f) || posted > until {
                    continue;
                }
                if claimed.contains(&t.id) || !t.description.to_lowercase().contains(&pattern) {
                    continue;
                }
                // A gross decomposition is only meaningful in the scale's own currency — the same
                // guard `plan_slices` applies to a scheduled deposit.
                if stream.basis.is_gross() && t.currency_code != stream.currency_code {
                    continue;
                }
                let breakdown = decompose(
                    stream,
                    posted,
                    t.amount_minor,
                    scales,
                    person_regular_annualised_on(streams, stream.ownership.person_id(), posted),
                );
                match self
                    .income
                    .record_variable_match(
                        stream.id,
                        &posted.to_string(),
                        t.id,
                        t.amount_minor,
                        &breakdown,
                    )
                    .await
                {
                    Ok(()) => {
                        claimed.insert(t.id);
                        matched += 1;
                    }
                    // The same-day collision above. Reported once per deposit rather than failing
                    // the run: every other deposit this stream has is still worth claiming.
                    Err(AppError::Conflict(_)) => tracing::warn!(
                        stream_id = stream.id,
                        transaction_id = t.id,
                        posted = %posted,
                        "a payment already exists for this stream on this date; leaving the \
                         second deposit for a person to link"
                    ),
                    Err(err) => return Err(err),
                }
            }
        }
        Ok(matched)
    }

    /// Re-derive the stored payslip of every settled payment whose stream's terms have moved
    /// under it, and return how many were rewritten.
    ///
    /// **Why a settled row is touched at all**, when the schedule regeneration above is careful
    /// never to disturb one. The two halves of a match are not the same kind of fact. *Which
    /// deposit satisfied which payday* is a decision — the matcher's, or a person's — and it
    /// stands until someone unlinks it. The decomposition beside it is not a decision: it is a
    /// pure function of the stream's terms on the due date, the scale in force that day, and the
    /// observed slice, and every one of those is something the stream's configuration supplies.
    /// Leave it alone across a config edit and the payslip layer silently mixes eras — after
    /// backdating a career's pay scale, the pays that were already matched keep the single
    /// KiwiSaver rate they were matched under, so two adjacent months on the chart itemise the
    /// same salary differently for no reason a reader can see.
    ///
    /// `expected_net_minor` is deliberately **not** recomputed, for exactly the reason the column
    /// documents: what the configuration predicted at match time is evidence about the drift, and
    /// rewriting it under the edited stream would erase the very gap it exists to show.
    ///
    /// Skips a gross-basis payment whose date has no scale on record rather than writing the
    /// passthrough `decompose` falls back to: replacing a real itemisation with "all gross, no
    /// tax" is strictly worse than leaving a stale one.
    async fn redecompose_settled(
        &self,
        settled: &[IncomePayment],
        streams: &[IncomeStream],
        scales: &TaxScales,
    ) -> AppResult<usize> {
        let by_id: HashMap<i64, &IncomeStream> = streams.iter().map(|s| (s.id, s)).collect();
        let mut rewritten = 0;
        for p in settled {
            if !matches!(
                p.status,
                IncomePaymentStatus::Matched | IncomePaymentStatus::Confirmed
            ) {
                continue;
            }
            let (Some(transaction_id), Some(observed), Some(stream)) = (
                p.transaction_id,
                p.observed_net_minor,
                by_id.get(&p.income_stream_id),
            ) else {
                continue;
            };
            let Some(due) = crate::reports::parse_date_pub(&p.due_on) else {
                continue;
            };
            if stream.basis.is_gross() && scales.at(due).is_none() {
                continue;
            }
            let fresh = decompose(
                stream,
                due,
                observed,
                scales,
                person_regular_annualised_on(streams, stream.ownership.person_id(), due),
            );
            if !breakdown_changed(p, &fresh) {
                continue;
            }
            self.income
                .record_payment_match(
                    stream.id,
                    &p.due_on,
                    transaction_id,
                    // Both preserved: this rewrites the arithmetic, never the decision or who
                    // made it. A confirmed match stays confirmed and stays the person's.
                    p.matched_by.unwrap_or(MatchedBy::Auto),
                    p.status,
                    observed,
                    &fresh,
                )
                .await?;
            rewritten += 1;
        }
        Ok(rewritten)
    }

    /// A person linking a deposit to a payment by hand — recorded as `confirmed` outright,
    /// because the person just did the confirming.
    ///
    /// The slice is whatever the deposit has left after every other payment already claiming
    /// it: linking the salary row and then the bonus row of one deposit works in either order,
    /// and the two slices always sum to the transaction.
    pub async fn link_manually(
        &self,
        payment_id: i64,
        transaction: &Transaction,
    ) -> AppResult<IncomePayment> {
        if transaction.amount_minor <= 0 {
            return Err(AppError::validation(
                "an income payment can only claim a deposit (a positive amount)",
            ));
        }
        let payment = self.income.get_income_payment(payment_id).await?;
        if payment.transaction_id.is_some() {
            return Err(AppError::conflict(
                "this payment already has a transaction — unlink it first",
            ));
        }
        let stream = self
            .income
            .get_income_stream(payment.income_stream_id)
            .await?;
        let Some(due) = crate::reports::parse_date_pub(&payment.due_on) else {
            return Err(AppError::Internal(anyhow::anyhow!(
                "income payment {payment_id} has an unparseable due_on {:?}",
                payment.due_on
            )));
        };
        if stream.basis.is_gross() && transaction.currency_code != stream.currency_code {
            return Err(AppError::validation(format!(
                "the deposit is in {} but the stream is priced in {} — a gross decomposition \
                 across currencies would be a guess",
                transaction.currency_code, stream.currency_code
            )));
        }

        let already_claimed: i64 = self
            .income
            .list_income_payments(None, None, None, None)
            .await?
            .iter()
            .filter(|p| p.transaction_id == Some(transaction.id) && p.id != payment_id)
            .filter_map(|p| p.observed_net_minor)
            .sum();
        let slice = transaction.amount_minor - already_claimed;
        if slice <= 0 {
            return Err(AppError::conflict(
                "other payments already claim this whole deposit",
            ));
        }

        let streams = self.income.list_income_streams().await?;
        let scales = TaxScales::new(&self.income.list_tax_scales().await?);
        // The bracket context as it was on the payment's *own* date, not today's — linking a pay
        // from two years ago by hand must itemise it the way payroll did that day.
        let breakdown = decompose(
            &stream,
            due,
            slice,
            &scales,
            person_regular_annualised_on(&streams, stream.ownership.person_id(), due),
        );
        self.income
            .record_payment_match(
                stream.id,
                &payment.due_on,
                transaction.id,
                MatchedBy::Manual,
                IncomePaymentStatus::Confirmed,
                slice,
                &breakdown,
            )
            .await
    }
}

/// Every place the matcher should look for this stream's deposits. Lower-cased here, once, so
/// grouping and candidate filtering cannot disagree about case; a blank pattern is dropped
/// rather than turned into a substring of every description (the DAL and the 0048 CHECK both
/// refuse one, so this is belt and braces against a hand-edited row).
fn match_keys(s: &IncomeStream) -> Vec<(i64, String)> {
    s.match_targets
        .iter()
        .filter_map(|t| {
            let pattern = t.pattern.trim().to_lowercase();
            (!pattern.is_empty()).then_some((t.account_id, pattern))
        })
        .collect()
}

/// What a stream's pay looked like on one date: the annual level, and the contribution rates the
/// deductions were taken at.
///
/// All three are dated, and for the same reason — a pay rise, a KiwiSaver re-election and the
/// 1 April 2026 default change are all "from this date, the pay is different". The residual
/// `annual_increase_bps` is deliberately not compounded into the level: it is a projection knob,
/// and the 2% match tolerance absorbs a raise for far longer than it takes the drift alert to
/// point at the real fix (a new step).
#[derive(Debug, Clone, Copy)]
struct Terms {
    level_minor: i64,
    kiwisaver_bps: i64,
    employer_kiwisaver_bps: i64,
}

/// The terms in force on `due` — the last dated step at or before it wins each field
/// independently, because a step that sets only a new salary must not reset a KiwiSaver election
/// an earlier step made.
fn terms_on(stream: &IncomeStream, due: NaiveDate) -> Terms {
    let mut terms = Terms {
        level_minor: stream.annual_amount_minor,
        kiwisaver_bps: stream.kiwisaver_bps,
        employer_kiwisaver_bps: stream.employer_kiwisaver_bps,
    };
    for step in &stream.steps {
        match crate::reports::parse_date_pub(&step.effective_on) {
            Some(d) if d <= due => {
                terms.level_minor = step.annual_amount_minor;
                if let Some(bps) = step.kiwisaver_bps {
                    terms.kiwisaver_bps = bps;
                }
                if let Some(bps) = step.employer_kiwisaver_bps {
                    terms.employer_kiwisaver_bps = bps;
                }
            }
            _ => {} // future or unparseable steps don't change the terms in force
        }
    }
    terms
}

/// `value / divisor` rounded half away from zero — payroll's own division, mirroring the
/// private helper in `sure_core::tax` for the one place this module divides a level itself.
fn div_round(value: i64, divisor: i64) -> i64 {
    let divisor = divisor.max(1);
    if value >= 0 {
        (value + divisor / 2) / divisor
    } else {
        (value - divisor / 2) / divisor
    }
}

/// One person's regular gross annual level **as it was on `on`** — the context an extra pay is
/// taxed on top of, and the whole-person-gross rule `crate::income::take_home` follows, because
/// PAYE brackets are progressive over total income.
///
/// Asked per due date rather than once for today, and filtered to the streams actually running
/// that day. Reading it at today's date priced a bonus paid two years ago against today's salary
/// and every job held since — for a backfill over a career that is the wrong bracket on every
/// historical extra pay, and it gets wronger the further back the history reaches.
///
/// `None` for the person (a joint stream) is 0: joint income is never gross, so it has no
/// marginal rate to pool.
fn person_regular_annualised_on(
    streams: &[IncomeStream],
    person_id: Option<i64>,
    on: NaiveDate,
) -> i64 {
    let Some(person_id) = person_id else {
        return 0;
    };
    streams
        .iter()
        .filter(|s| {
            s.enabled
                && s.basis.is_gross()
                && s.pay_treatment == PayTreatment::Regular
                && s.ownership.person_id() == Some(person_id)
                && crate::reports::parse_date_pub(&s.starts_on).is_none_or(|d| d <= on)
                && crate::reports::parse_date_pub(s.ends_on.as_deref().unwrap_or(""))
                    .is_none_or(|d| d >= on)
        })
        .map(|s| terms_on(s, on).level_minor)
        .sum()
}

/// What one payday of `stream` should net, on `due`, under the scale in force that day.
fn expected_net(
    stream: &IncomeStream,
    due: NaiveDate,
    scales: &TaxScales,
    person_regular_minor: i64,
) -> i64 {
    let terms = terms_on(stream, due);
    let per_payment = div_round(
        terms.level_minor,
        stream.pay_frequency.periods_per_year_int(),
    );
    match stream.basis {
        IncomeBasis::Net => per_payment,
        IncomeBasis::GrossNzPaye => {
            let Some(resolved) = scales.at(due) else {
                // No scale on record for this date: predicting an untaxed landing would be
                // wrong by a third; predicting the gross at least matches nothing silently.
                return per_payment;
            };
            let scale = resolved.as_scale();
            match stream.pay_treatment {
                PayTreatment::Regular => {
                    tax::paye_period(
                        &scale,
                        PeriodPayeInput {
                            period_gross_minor: per_payment,
                            periods_per_year: stream.pay_frequency.periods_per_year_int(),
                            kiwisaver_bps: terms.kiwisaver_bps,
                            employer_kiwisaver_bps: terms.employer_kiwisaver_bps,
                            student_loan: stream.student_loan,
                        },
                    )
                    .net_minor
                }
                PayTreatment::ExtraPay => {
                    tax::extra_pay(
                        &scale,
                        ExtraPayInput {
                            annualised_regular_minor: person_regular_minor,
                            extra_minor: per_payment,
                            kiwisaver_bps: terms.kiwisaver_bps,
                            employer_kiwisaver_bps: terms.employer_kiwisaver_bps,
                            student_loan: stream.student_loan,
                        },
                    )
                    .net_minor
                }
            }
        }
    }
}

/// The reconstructed decomposition of one stream's `slice` of a deposit landed against `due`.
///
/// Gross NZ PAYE runs the statutory inverse so the lines reconcile to the observed slice
/// exactly; a net-basis stream (an untaxed reimbursement, a supplement paid outside payroll)
/// passes through whole — its gross *is* what landed.
fn decompose(
    stream: &IncomeStream,
    due: NaiveDate,
    slice_minor: i64,
    scales: &TaxScales,
    person_regular_minor: i64,
) -> PayeBreakdown {
    let passthrough = PayeBreakdown {
        gross_minor: slice_minor,
        net_minor: slice_minor,
        ..Default::default()
    };
    let terms = terms_on(stream, due);
    match stream.basis {
        IncomeBasis::Net => passthrough,
        IncomeBasis::GrossNzPaye => {
            let Some(resolved) = scales.at(due) else {
                return passthrough;
            };
            let scale = resolved.as_scale();
            match stream.pay_treatment {
                PayTreatment::Regular => tax::reconstruct_period(
                    &scale,
                    slice_minor,
                    PeriodPayeInput {
                        period_gross_minor: 0,
                        periods_per_year: stream.pay_frequency.periods_per_year_int(),
                        kiwisaver_bps: terms.kiwisaver_bps,
                        employer_kiwisaver_bps: terms.employer_kiwisaver_bps,
                        student_loan: stream.student_loan,
                    },
                ),
                // The bracket a bonus starts in is set by the person's whole regular gross —
                // the configured level, the same stand-in `paye`'s ESCT makes for IR's
                // "prior year plus expected" figure.
                PayTreatment::ExtraPay => tax::reconstruct_extra_pay(
                    &scale,
                    slice_minor,
                    ExtraPayInput {
                        annualised_regular_minor: person_regular_minor,
                        extra_minor: 0,
                        kiwisaver_bps: terms.kiwisaver_bps,
                        employer_kiwisaver_bps: terms.employer_kiwisaver_bps,
                        student_loan: stream.student_loan,
                    },
                ),
            }
        }
    }
}

/// The best unclaimed candidate for a deposit expected on `due` at `predicted` net: inside
/// `[due − DAYS_EARLY, due + DAYS_LATE]`, closest by amount then by date, and within tolerance.
fn best_candidate<'t>(
    candidates: &'t [Transaction],
    claimed: &HashSet<i64>,
    due: NaiveDate,
    predicted: i64,
) -> Option<&'t Transaction> {
    let tolerance = TOLERANCE_FLOOR_MINOR.max(div_round(predicted * TOLERANCE_BPS, 10_000));
    candidates
        .iter()
        .filter(|t| !claimed.contains(&t.id))
        .filter_map(|t| {
            let posted = crate::reports::parse_date_pub(&t.posted_at)?;
            let offset = (due - posted).num_days();
            if !(-DAYS_LATE..=DAYS_EARLY).contains(&offset) {
                return None;
            }
            let miss = (t.amount_minor - predicted).abs();
            if miss > tolerance {
                return None;
            }
            Some((miss, offset.abs(), t))
        })
        .min_by_key(|&(miss, offset, _)| (miss, offset))
        .map(|(_, _, t)| t)
}

/// How a deposit divides among the streams expecting it on one date.
///
/// Handles the shapes payroll actually produces — one stream alone, or one regular salary plus
/// one extra pay in the same run — and declines anything else (two salaries in one deposit has
/// no principled split; a person can link those by hand). For the salary+bonus case the base
/// slice follows the stream's most recent observed regular pay (else its prediction) and the
/// bonus is the residual — one observation cannot pin two unknowns, so the recent pay is the
/// anchor (`reconstruct` then inverts each slice separately). A residual that is negative or
/// implausibly far above the configured bonus leaves the whole deposit for review.
fn plan_slices<'s>(
    contributors: &[&'s IncomeStream],
    expected: &HashMap<i64, BTreeMap<NaiveDate, i64>>,
    last_observed: &HashMap<i64, i64>,
    due: NaiveDate,
    observed: i64,
    tx_currency: &str,
) -> Option<Vec<(&'s IncomeStream, i64)>> {
    // A gross decomposition is only meaningful in the scale's own currency; a mismatched
    // deposit is left for a person to judge.
    if contributors
        .iter()
        .any(|s| s.basis.is_gross() && s.currency_code != tx_currency)
    {
        return None;
    }
    let predicted_of = |s: &IncomeStream| expected.get(&s.id).and_then(|d| d.get(&due)).copied();
    match contributors {
        [only] => Some(vec![(only, observed)]),
        [a, b] => {
            let (base, extra) = match (a.pay_treatment, b.pay_treatment) {
                (PayTreatment::Regular, PayTreatment::ExtraPay) => (a, b),
                (PayTreatment::ExtraPay, PayTreatment::Regular) => (b, a),
                // Two regulars or two extras on one deposit: no principled split.
                (PayTreatment::Regular, PayTreatment::Regular)
                | (PayTreatment::ExtraPay, PayTreatment::ExtraPay) => return None,
            };
            let base_slice = last_observed
                .get(&base.id)
                .copied()
                .or_else(|| predicted_of(base))?;
            let extra_slice = observed - base_slice;
            let extra_pred = predicted_of(extra)?;
            let ceiling = extra_pred + div_round(extra_pred * EXTRA_HEADROOM_BPS, 10_000);
            if extra_slice <= 0 || extra_slice > ceiling {
                return None;
            }
            Some(vec![(*base, base_slice), (*extra, extra_slice)])
        }
        _ => None, // three-plus streams in one deposit is nothing payroll produces
    }
}

/// Whether a freshly derived payslip says anything different from the one already stored.
///
/// Compared field by field against the `Option` columns so a row written before a component
/// existed (all `None`) reads as changed and is filled in, rather than being left half-empty
/// because the two happened to agree on the fields that were there.
fn breakdown_changed(stored: &IncomePayment, fresh: &PayeBreakdown) -> bool {
    [
        (stored.gross_minor, fresh.gross_minor),
        (stored.income_tax_minor, fresh.income_tax_minor),
        (stored.acc_levy_minor, fresh.acc_levy_minor),
        (stored.kiwisaver_minor, fresh.kiwisaver_minor),
        (stored.student_loan_minor, fresh.student_loan_minor),
        (
            stored.employer_kiwisaver_minor,
            fresh.employer_kiwisaver_minor,
        ),
        (stored.esct_minor, fresh.esct_minor),
    ]
    .iter()
    .any(|(stored, fresh)| *stored != Some(*fresh))
}

/// The most recent observed net per *regular* stream, from the settled rows — the base-slice
/// anchor for bonus-quarter deposits.
fn latest_observed_by_stream(payments: Vec<IncomePayment>) -> HashMap<i64, i64> {
    let mut latest: HashMap<i64, (String, i64)> = HashMap::new();
    for p in payments {
        if !matches!(
            p.status,
            IncomePaymentStatus::Matched | IncomePaymentStatus::Confirmed
        ) {
            continue;
        }
        let Some(observed) = p.observed_net_minor else {
            continue;
        };
        let newer = latest
            .get(&p.income_stream_id)
            .is_none_or(|(due, _)| *due < p.due_on);
        if newer {
            latest.insert(p.income_stream_id, (p.due_on, observed));
        }
    }
    latest.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::{IncomeStreamStep, PayFrequency};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn scales() -> TaxScales {
        TaxScales::new(&[])
    }

    fn stream(id: i64, treatment: PayTreatment) -> IncomeStream {
        IncomeStream {
            id,
            ownership: sure_core::Ownership::Person { person_id: 1 },
            label: format!("Stream {id}"),
            employer: None,
            currency_code: "NZD".into(),
            annual_amount_minor: 96_000_00,
            basis: IncomeBasis::GrossNzPaye,
            pay_frequency: PayFrequency::SemiMonthly,
            first_payment_on: "2026-01-14".into(),
            starts_on: "2026-01-01".into(),
            ends_on: None,
            annual_increase_bps: 0,
            kiwisaver_bps: 350,
            employer_kiwisaver_bps: 350,
            student_loan: true,
            take_home_bps: None,
            linked_category_id: None,
            kiwisaver_account_id: None,
            student_loan_account_id: None,
            match_targets: vec![sure_core::IncomeStreamMatchTarget {
                id,
                income_stream_id: id,
                account_id: 7,
                pattern: "KAIMAHI".into(),
            }],
            pay_treatment: treatment,
            pay_pattern: sure_core::PayPattern::Scheduled,
            enabled: true,
            sort_order: 0,
            notes: None,
            steps: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn step(effective_on: &str, annual_amount_minor: i64) -> IncomeStreamStep {
        IncomeStreamStep {
            id: 1,
            income_stream_id: 1,
            effective_on: effective_on.into(),
            annual_amount_minor,
            label: None,
            kiwisaver_bps: None,
            employer_kiwisaver_bps: None,
        }
    }

    fn tx(id: i64, posted_at: &str, amount_minor: i64) -> Transaction {
        Transaction {
            id,
            account_id: 7,
            posted_at: posted_at.into(),
            amount_minor,
            currency_code: "NZD".into(),
            description: "KAIMAHI COLLECTIVE SALARY 042".into(),
            merchant: None,
            merchant_id: None,
            notes: None,
            category_id: None,
            is_one_off: false,
            linked_transaction_id: None,
            provider: None,
            external_id: None,
            categorized_by_rule_id: None,
            ownership: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// The expected net of a payday is the statutory per-period figure — and a dated step
    /// changes it from exactly its effective date, not from the start of a tax year.
    #[test]
    fn expected_net_follows_the_step_in_force() {
        let mut s = stream(1, PayTreatment::Regular);
        s.steps = vec![step("2026-07-01", 120_000_00)];
        let before = expected_net(&s, d("2026-06-14"), &scales(), 96_000_00);
        let after = expected_net(&s, d("2026-07-14"), &scales(), 96_000_00);
        assert!(after > before, "a raise must raise the prediction");
        // The before figure is the hand-computed payslip from the sure-core tests.
        assert_eq!(before, 4_000_00 - 898_23 - 70_00 - 359_36 - 140_00);
    }

    /// A net-basis stream (a supplement paid outside payroll) predicts and decomposes as
    /// itself: no deductions to reconstruct.
    #[test]
    fn a_net_stream_passes_through_whole() {
        let mut s = stream(1, PayTreatment::Regular);
        s.basis = IncomeBasis::Net;
        s.annual_amount_minor = 1_800_00;
        s.pay_frequency = PayFrequency::Monthly;
        assert_eq!(expected_net(&s, d("2026-06-01"), &scales(), 0), 150_00);
        let b = decompose(&s, d("2026-06-01"), 150_00, &scales(), 0);
        assert_eq!(b.gross_minor, 150_00);
        assert_eq!(b.net_minor, 150_00);
        assert_eq!(b.income_tax_minor, 0);
    }

    /// Candidate choice prefers the closest amount inside the window, leans early, and
    /// refuses anything outside tolerance.
    /// A stream may be matched in more than one account, because a job outlives a bank account.
    /// Both targets are searched, and the memo token may differ between them — a bank rewriting
    /// its statement format is the other half of the same problem.
    #[test]
    fn a_stream_is_matched_everywhere_its_targets_point() {
        let mut s = stream(1, PayTreatment::Regular);
        s.match_targets = vec![
            sure_core::IncomeStreamMatchTarget {
                id: 1,
                income_stream_id: 1,
                account_id: 7,
                pattern: "ACME LTD SALARY/WAGES PAY".into(),
            },
            sure_core::IncomeStreamMatchTarget {
                id: 2,
                income_stream_id: 1,
                account_id: 9,
                pattern: "ACME LIM SALARY/WAGESPAY".into(),
            },
        ];
        assert_eq!(
            match_keys(&s),
            vec![
                (7, "acme ltd salary/wages pay".to_string()),
                (9, "acme lim salary/wagespay".to_string()),
            ],
            "every target is a place to look, lower-cased once"
        );
        // No targets is matching off, and is the only thing that turns it off.
        s.match_targets.clear();
        assert!(match_keys(&s).is_empty());
    }

    /// A step's contribution election applies from its own date and leaves the level alone, and a
    /// later step that says nothing about KiwiSaver does not reset it.
    #[test]
    fn a_kiwisaver_election_is_dated_independently_of_the_level() {
        let mut s = stream(1, PayTreatment::Regular);
        s.kiwisaver_bps = 300;
        s.employer_kiwisaver_bps = 300;
        s.steps = vec![
            IncomeStreamStep {
                kiwisaver_bps: Some(350),
                employer_kiwisaver_bps: Some(350),
                ..step("2026-04-01", 108_000_00)
            },
            // A pay rise, and nothing else: the 3.5% election survives it.
            step("2026-07-01", 110_400_00),
        ];

        let before = terms_on(&s, d("2026-03-31"));
        assert_eq!(before.level_minor, 96_000_00);
        assert_eq!(
            before.kiwisaver_bps, 300,
            "the stream's own rate still applies"
        );

        let after = terms_on(&s, d("2026-04-14"));
        assert_eq!(after.level_minor, 108_000_00);
        assert_eq!(after.kiwisaver_bps, 350);
        assert_eq!(after.employer_kiwisaver_bps, 350);

        let later = terms_on(&s, d("2026-07-14"));
        assert_eq!(later.level_minor, 110_400_00);
        assert_eq!(
            later.kiwisaver_bps, 350,
            "a pay rise does not undo an election"
        );
    }

    /// The election is what the deposit is decomposed at, which is the whole reason it is dated:
    /// reconstructing an old pay at today's rate still reconciles to the cent, so the error hides
    /// in the split between the KiwiSaver line and PAYE rather than showing up as a mismatch.
    #[test]
    fn the_dated_election_is_what_the_decomposition_uses() {
        let mut s = stream(1, PayTreatment::Regular);
        s.kiwisaver_bps = 300;
        s.employer_kiwisaver_bps = 300;
        s.steps = vec![IncomeStreamStep {
            kiwisaver_bps: Some(350),
            employer_kiwisaver_bps: Some(350),
            ..step("2026-04-01", 96_000_00)
        }];

        let old = decompose(&s, d("2026-03-14"), 3_000_00, &scales(), 0);
        let new = decompose(&s, d("2026-04-14"), 3_000_00, &scales(), 0);
        assert_eq!(old.net_minor, 3_000_00);
        assert_eq!(
            new.net_minor, 3_000_00,
            "both still reconcile to the deposit"
        );
        assert!(
            new.kiwisaver_minor > old.kiwisaver_minor,
            "3.5% of gross must be more than 3%: {} vs {}",
            new.kiwisaver_minor,
            old.kiwisaver_minor
        );
        // Every line still adds up, at both rates — the residual lands in income tax.
        for b in [old, new] {
            assert_eq!(
                b.gross_minor
                    - b.income_tax_minor
                    - b.acc_levy_minor
                    - b.kiwisaver_minor
                    - b.student_loan_minor,
                b.net_minor
            );
        }
    }

    /// An extra pay is taxed on top of the person's regular gross **as it was that day**. Reading
    /// it at today's date prices a bonus paid before a raise at the post-raise bracket.
    #[test]
    fn the_bracket_context_is_the_payments_own_date() {
        let mut salary = stream(1, PayTreatment::Regular);
        salary.annual_amount_minor = 60_000_00;
        salary.steps = vec![step("2026-07-01", 180_000_00)];
        let streams = vec![salary];

        let before = person_regular_annualised_on(&streams, Some(1), d("2026-06-30"));
        let after = person_regular_annualised_on(&streams, Some(1), d("2026-07-01"));
        assert_eq!(before, 60_000_00);
        assert_eq!(after, 180_000_00);

        // A stream that had not started, or has ended, contributes nothing to the bracket on
        // that date — it was not being paid.
        let mut ended = stream(2, PayTreatment::Regular);
        ended.annual_amount_minor = 40_000_00;
        ended.starts_on = "2026-01-01".into();
        ended.ends_on = Some("2026-03-31".into());
        let streams = vec![ended];
        assert_eq!(
            person_regular_annualised_on(&streams, Some(1), d("2026-02-14")),
            40_000_00
        );
        assert_eq!(
            person_regular_annualised_on(&streams, Some(1), d("2026-06-14")),
            0
        );

        // Joint income has no person, so it has no marginal rate to pool.
        assert_eq!(
            person_regular_annualised_on(&streams, None, d("2026-02-14")),
            0
        );
    }

    #[test]
    fn the_best_candidate_is_the_closest_amount_inside_the_window() {
        let predicted = 2_532_41;
        let pool = vec![
            tx(1, "2026-06-12", 2_532_41), // two days early, exact
            tx(2, "2026-06-14", 2_600_00), // on the day, $68 off — outside 2%
            tx(3, "2026-06-05", 2_532_41), // exact but far too early
        ];
        let claimed = HashSet::new();
        let best = best_candidate(&pool, &claimed, d("2026-06-14"), predicted).unwrap();
        assert_eq!(best.id, 1);
        // Claiming it leaves nothing acceptable.
        let claimed: HashSet<i64> = [1].into();
        assert!(best_candidate(&pool, &claimed, d("2026-06-14"), predicted).is_none());
    }

    /// One deposit, salary plus bonus: the base slice follows the most recent observed
    /// regular pay and the bonus is the residual — and a residual past the headroom leaves
    /// the whole deposit alone.
    #[test]
    fn a_bonus_deposit_splits_base_from_residual() {
        let base = stream(1, PayTreatment::Regular);
        let extra = stream(2, PayTreatment::ExtraPay);
        let mut expected = HashMap::new();
        expected.insert(1, BTreeMap::from([(d("2026-06-14"), 2_532_41i64)]));
        expected.insert(2, BTreeMap::from([(d("2026-06-14"), 1_400_00i64)]));
        let last = HashMap::from([(1i64, 2_531_00i64)]);

        let contributors = [&base, &extra];
        let slices = plan_slices(
            &contributors,
            &expected,
            &last,
            d("2026-06-14"),
            3_950_00,
            "NZD",
        )
        .unwrap();
        assert_eq!(
            slices[0].1 + slices[1].1,
            3_950_00,
            "slices cover the deposit"
        );
        assert_eq!(slices[0].1, 2_531_00, "base follows the observed pay");

        // A deposit smaller than the base alone: the residual would be negative.
        assert!(
            plan_slices(
                &contributors,
                &expected,
                &last,
                d("2026-06-14"),
                2_000_00,
                "NZD",
            )
            .is_none()
        );
        // A gross stream cannot decompose a foreign-currency deposit.
        assert!(
            plan_slices(
                &contributors,
                &expected,
                &last,
                d("2026-06-14"),
                3_950_00,
                "USD",
            )
            .is_none()
        );
    }

    /// The reconstructed slices reconcile: every line of both breakdowns sums back to the
    /// observed deposit, which is what lets the sankey balance to the cent.
    #[test]
    fn decomposed_slices_reconcile_to_the_deposit() {
        let base = stream(1, PayTreatment::Regular);
        let mut extra = stream(2, PayTreatment::ExtraPay);
        extra.pay_frequency = PayFrequency::Quarterly;
        extra.annual_amount_minor = 10_000_00;
        let scales = scales();
        let observed_base = 2_532_41;
        let observed_extra = 1_417_59;

        let b1 = decompose(&base, d("2026-06-14"), observed_base, &scales, 96_000_00);
        let b2 = decompose(&extra, d("2026-06-14"), observed_extra, &scales, 96_000_00);
        for b in [&b1, &b2] {
            assert_eq!(
                b.gross_minor
                    - b.income_tax_minor
                    - b.acc_levy_minor
                    - b.kiwisaver_minor
                    - b.student_loan_minor,
                b.net_minor
            );
        }
        assert_eq!(b1.net_minor + b2.net_minor, observed_base + observed_extra);
        // An extra pay's student loan has no threshold: 12% of its whole gross.
        assert_eq!(
            b2.student_loan_minor,
            div_round(b2.gross_minor * 1_200, 10_000)
        );
    }
}
