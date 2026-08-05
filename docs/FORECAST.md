# Forecast

A Monte Carlo projection of the household's net worth, plus the two models that drive it: what
people earn, and what might happen to them.

Assumption resolution and the simulation live in `sure_app::forecast`; the income calendar and
gross→net map in `sure_app::income`; NZ deduction rules in `sure_core::tax`. Nothing here writes to
the ledger — unlike `crons`, which persists real rows.

## Where each number comes from

| Number | Source |
|---|---|
| An account's growth / volatility | an override, else an enabled appreciation/depreciation/interest cron's rate, else fitted from up to 36 months of its own value series |
| A mortgage/loan's balance | its own amortisation schedule, exactly — no rate to resolve |
| A category's monthly baseline | mean of up to 24 trailing complete months of its own spend |
| A category with linked income streams | the **residual**: fitted baseline minus what the streams model |
| A salary's take-home | an override, else "already net", else `sure_core::tax`'s dated statutory scale |
| When an event happens | sampled per path from a uniform hard window around `expected_on` |

## The traps, and why the code looks the way it does

**Double counting income.** A stream already landing in the bank is *also* inside the fitted
baseline of the category it lands in. `income_streams.linked_category_id` is what prevents counting
it twice — the category's baseline becomes the residual, and `reconciliations` reports modelled
against recorded so a mistake is visible. Netting rather than excluding, because excluding would
silently drop the income the streams do not explain (interest, a gift, an unmodelled second job).

**Coverage over 100% is the gross/net mistake.** A modelled figure a fifth to a half above what the
category recorded is the signature of a salary entered before tax and modelled as take-home. The
residual floors at zero but the coverage figure is not clamped, because that figure is the warning.

**A month is not a pay period.** A fortnightly payer is paid 26 times a year, which is not twice a
month. Payments are enumerated from `first_payment_on`, so three-payday months land where they
really land, four-weekly streams drift (13 × 28 = 364), and a 31st-of-the-month anchor pays on the
30th in April. The window is whole calendar months at both ends — anchoring the end on today's
day-of-month silently drops late paydays in the final month.

**Tax is per person, not per stream.** Brackets are progressive over total income, so two salaries
priced separately would each be taxed as if the other did not exist. The take-home *ratio* comes
from the annual level and the month's amount from the calendar; annualising the month would shove a
quarterly bonus into the top bracket for that month alone.

**Marginal ≠ average.** A promotion's increment is taxed at the bracket the salary reaches. `TakeHome`
carries both rates for exactly this reason, and `marginal_take_home_bps` is *differenced from the
real function* rather than looked up, because the ACC cap makes the combination non-monotonic: a
raise above the cap keeps more than one just below it.

**Events must not perturb the projection's RNG.** Each event draws from its own stream, seeded per
`(event, path)`. With no events configured, every figure is byte-identical to a run from before they
existed — and adding or reordering an event cannot move another event's realisation.

**Relations clamp, never resample.** A reject-and-retry makes the number of draws depend on the
draws, and a seeded run stops being reproducible. Only `after` and `only_if` are stored, and `after`
is one-directional so the edge set *is* the dependency graph; the UI offers "before" and writes the
reversed edge.

**A sampled month in the past clamps to month 1, not 0.** Month 0 is today, already inside the
history every baseline was fitted from, so firing there would double-apply. `clamped_early_rate_bps`
reports when this happened.

**A 24-month fit is not evidence about year twenty-nine.** Past month 60 a *derived* rate decays
toward its long-run anchor with a 24-month half-life. At the derived growth ceiling that is ×5.97
over thirty years instead of ×807; an ordinary +3%/yr goes ×1.26 instead of ×2.43. Overrides and
cron rates are not decayed — those are assertions.

**The chart draws realised timing, not configured timing.** A relation can push an event years past
its expected date. Drawing `expected_on` would misrepresent the one thing the chart exists to show.

## What the model deliberately does not carry

- **Per-person expenses.** Income is attributed; spending is the household's.
- **Overdraft interest.** A negative cash pool is filed under liabilities by sign and reported via
  `negative_cash_rate_bps`, but costs nothing to hold.
- **KiwiSaver and student-loan destinations.** Both are deducted correctly, but the contributions do
  not yet credit a KiwiSaver account or pay down the student loan. Doing so requires switching that
  account off its fitted trend at the same time, or the repayments are counted twice — the trap
  `AccountSim::repayment_debits_cash` documents.
- **A jurisdiction other than New Zealand.** `IncomeBasis::Net` / `TaxScaleId::None` is the escape
  hatch: record take-home and no scale is applied.
- **Scenarios.** There is one plan, not a set to compare. `enabled` on a stream and
  `probability_bps: 0` on an event are the closest thing.

## Operational notes

- `MAX_HORIZON_MONTHS` is 360, and paths trade against months under `MAX_PATH_MONTHS`, so a 30-year
  run costs about what a 5-year one does. `ForecastResult` echoes the `horizon_months` and
  `simulations` actually run — a caller asking for more can tell.
- `GET /api/forecast` is in `LONG_ROUTES`: at 360 months it is seconds of CPU on the blocking pool.
- Tax figures in `sure_core::tax` are dated and append-only. Editing an entry restates a tax year
  that has already happened; add a new scale instead, and record where the figures came from.
