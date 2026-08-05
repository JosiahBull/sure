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
| A salary's take-home | an override, else "already net", else the **stored** tax scale in force on the date (`sure_core::tax`'s constants seed it and are the fallback) |
| A KiwiSaver balance's growth | its own rate is discarded when linked; less any fund fee on the assumption |
| The government's KiwiSaver contribution | matched against the member's own contributions only, capped, and income-tested |
| When an event happens | sampled per path from a uniform hard window around `expected_on` |
| A KiwiSaver balance | its own growth rate is *discarded* when linked (see below); contributions credited monthly |
| A student loan's paydown | the deductions themselves, plus `StudentLoanMeta::interest_rate_bps` (0 for an NZ-based borrower) |

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

**A fitted rate on an account receiving contributions is flattering.** A KiwiSaver balance rising
15%/yr might be 8% market and 7% contributions, and nothing can separate them from a balance series
after the fact. So linking a contribution target *discards* that rate: growth comes from an override,
else the long-run anchor, else flat, reported as `contribution_driven` with a warning. The measured
volatility is kept — the scatter is real either way. A consequence worth expecting: linking often
makes the projection **smaller**, because the honest flat rate plus real contributions is less than
the flattering rate was.

**Twice a month is not fortnightly.** Twice a month is 24 payments a year; every fourteen days is
26. People describe both as "fortnightly", and on a $135,000 salary the difference is $5,625 a
payslip against $5,192. They are told apart by *shape*, not by average: twice-monthly alternates long
and short gaps and lands on the same two days every month, where fortnightly walks through the
calendar. `GET /api/income-streams/detect` reads which one someone is actually on out of the ledger,
along with the day it lands and the net figure — the three details people most often get wrong when
typing a salary in by hand.

**Tax rules are data, not constants.** IRD moves a threshold and a projection is quietly wrong until
someone ships a binary, so scales are stored, dated and editable. The constants remain the *seed and
fallback*: `migrate` copies them into an empty table in Rust rather than via INSERTs in the
migration, so there is exactly one place the figures are written down and no SQL copy free to drift.
Seeding never overwrites, which is what lets an edited rate survive an upgrade; `restore` is the
explicit way back. Deleting the last scale is refused — an empty table taxes every gross salary at
nothing, which reads as a windfall.

**ESCT comes off the employer's contribution, not on top of it.** business.govt.nz: "the tax you take
off the cash contributions you make". The account receives contribution × (1 − ESCT). Getting this
backwards overstates a KiwiSaver balance by up to 39% of every employer dollar. ESCT is a *flat* rate
chosen by which bracket the total lands in, not a progressive slice, and its thresholds sit exactly
20% above the PAYE ones.

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
- **A jurisdiction other than New Zealand.** `IncomeBasis::Net` / `TaxScaleId::None` is the escape
  hatch: record take-home and no scale is applied.
- **Prorating the government contribution.** Not adjusted for a partial membership year or for
  someone under 18 or over 65 — a projection is about whole years, and those rules need a birthday
  the model does not carry.
- **A KiwiSaver account's expected return.** It has to be set by hand once linked, because the only
  rate the data could offer is the contaminated one.
- **Employer contributions above the compulsory minimum varying over time.** One rate per stream.
- **Fees that change with the balance.** One percentage and one flat amount per account; tiered fee
  schedules are not modelled.
- **Scenarios.** There is one plan, not a set to compare. `enabled` on a stream and
  `probability_bps: 0` on an event are the closest thing.

## Operational notes

- `MAX_HORIZON_MONTHS` is 360, and paths trade against months under `MAX_PATH_MONTHS`, so a 30-year
  run costs about what a 5-year one does. `ForecastResult` echoes the `horizon_months` and
  `simulations` actually run — a caller asking for more can tell.
- `GET /api/forecast` is in `LONG_ROUTES`: at 360 months it is seconds of CPU on the blocking pool.
- Tax figures in `sure_core::tax` are dated and append-only. Editing an entry restates a tax year
  that has already happened; add a new scale instead, and record where the figures came from.
