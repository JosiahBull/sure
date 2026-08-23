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
| A category's monthly baseline | mean of the trailing 12 complete months of its **current regime** — see "A level is not a trend" |
| A category's growth | the one household rate, `settings.inflation_bps` (default 250 bps), unless overridden — never fitted |
| A category's growth override | absolute, or `settings.inflation_bps` + the stored spread when `growth_is_real` |
| A category with commitments | `Σ stated commitments` (deterministic, escalating) + a stochastic residual |
| A category with linked income streams | the **residual**: fitted baseline minus what the streams model |
| A category carrying a loan's interest | the **residual**: fitted baseline minus the interest that loan's schedule charges |
| An income stream's growth after its last dated step | `annual_increase_bps`, plus `settings.inflation_bps` when the stream is `inflation_indexed` |
| A salary's take-home | an override, else "already net", else the **stored** tax scale in force on the date (`sure_core::tax`'s constants seed it and are the fallback) |
| A KiwiSaver balance's growth | its own rate is discarded when linked; less any fund fee on the assumption |
| The government's KiwiSaver contribution | matched against the member's own contributions only, capped, and income-tested |
| When an event happens | sampled per path from a uniform hard window around `expected_on` |
| A KiwiSaver balance | its own growth rate is *discarded* when linked (see below); contributions credited monthly |
| A student loan's paydown | the deductions themselves, plus `StudentLoanMeta::interest_rate_bps` (0 for an NZ-based borrower) |

## The traps, and why the code looks the way it does

**A level is not a trend, and a window spanning a house purchase confuses the two.** A category's
growth used to be fitted by OLS over 24 months of its own spend. On a household that bought a house
mid-window this produced, for six of seven categories, fitted rates between +43%/yr and +126%/yr —
each of them one level shift read as a compounding rate, because across a window containing a step
a step genuinely *is* a monotone rise and passes a significance test honestly. All six were clamped
to the ±25%/yr derived ceiling, which over 360 months multiplies spending by 5.97. A ceiling that
six of seven categories sit exactly on is no longer a guard against over-fitting; it *is* the model,
and it is a number nobody chose. The same window simultaneously understated their current spending
by 38% ($2,515/mo against $4,051/mo), because a mean across two regimes describes neither.

Three changes, and the third is the one that matters:

1. **Leading structural zeros leave the window.** The series is anchored on a category's first
   activity, so one stray early transaction dragged the window back over a year of months in which
   the category did not exist yet. A leading run longer than three months is now dropped.
2. **One structural break is detected and everything before it discarded** (`strongest_mean_break`).
   Binary segmentation on the mean, gated on a Chow F ≥ 5.0 *and* on the step model beating a single
   linear trend at equal parameter cost. That second gate is what stops a genuinely accelerating
   category being flattened into a step — a ramp's best mean-split has a large F, because half of a
   rising line does sit above the other half. The level is then the trailing 12 months of what
   survives, which keeps a category that *ramped* into its current level anchored near where it
   ended up rather than near the middle of the ramp.
3. **Growth is not fitted at all.** It is `settings.inflation_bps`, one household rate, shown on the
   Assumptions tab with the dollars it implies at the horizon printed beneath it. 24 lumpy months
   estimate a category's mean to perhaps ±15% and do not identify its trend; assuming the trend and
   measuring the level is the honest division. The measured slope is still reported, as
   `measured_growth_bps`, so a reader can see what their history says — but nothing in the
   simulation reads it, and `AssumptionSource::Indexed` says so on the row.

Note what would *not* have fixed this: a robust estimator. Theil–Sen on those same six series
returns essentially the same slopes as OLS (38.2 against 41.8 on Household), because robust
regression is robust to outliers and a level shift is not an outlier.

**Income indexes on the same dial, or the fix is worse than the bug.** Inflating expenses while
income stays frozen in nominal terms removes the overshoot and keeps the deficit — net worth still
flattens, just for a new reason. `income_streams.inflation_indexed` opts a stream in;
`annual_increase_bps` then becomes a *real* increase stacked on top of the household rate ("CPI +
1%"), the same convention a per-category override follows, so the two dials compose instead of
contradicting each other. With it off the field keeps its original nominal meaning, so the column
arriving restates nobody's projection.

It defaults to **off**, and the projection instead emits a warning naming every frozen stream and
the figure it is frozen at. Defaulting to on would silently restate every existing user's numbers on
migration, which is the objection this document already records against projecting a
contribution-driven account at an invented rate. The warning is silent when the household rate is
zero — the two sides then agree, and a frozen level in a world with no inflation is just a level.

**A category override can be a spread rather than an absolute rate.** With
`forecast_assumptions.growth_is_real` set, `annual_growth_bps` reads as a rate *above*
`settings.inflation_bps` — "childcare runs 3% above everything else" — and the row shows both the
sum that ran and the spread that was asserted. The reason is that the household rate is a number
people revise, and an absolute per-category override is a copy of it that does not get revised with
it: written as a spread, three opinions about childcare, rates and insurance stay correct when the
dial moves; written as absolutes, all three silently become wrong. Defaults to absolute, so every
override stored before this keeps meaning what it meant. Categories only — an account's growth is a
market return, not a spread over household CPI.

**Commitments: the deterministic half of spending.** `expense_commitments` holds a recurring
obligation with a *stated* amount — rates, insurance, power, internet, a subscription — on a
cadence, escalating at the household rate plus its own delta, optionally ending. A category is then
`Σ commitments + a stochastic residual`, and only the residual gets a volatility. A power bill has
no volatility and no fitted trend; it has a price, an escalation clause and sometimes an end date,
and a fixed term *ending* is the one behaviour no fitted trend can represent at all.

The netting is where this can go wrong, so the rule is a type rather than a comment
(`CommitmentNetting`): a commitment is **either** removed from the fitted series by `merchant_id`
**or** subtracted from the fitted level, never both and never neither. Both, and the money leaves
twice — the same defect as the mortgage interest above, which reached production because the
reasoning that ruled it out lived in a comment that was wrong. The two mechanisms are not equal in
value: exclusion narrows the residual's *volatility* as well as its level, and subtraction only
corrects the level, so exclusion is used wherever a merchant makes exact identification possible.
Matching by amount instead would silently swallow a grocery shop that happened to cost $250. Only a
commitment already running nets against history — a contract starting in seven months was never
inside the window the baseline was measured over, the same rule `active_from <= 1` applies to an
income stream.

Two guards, both added after measuring the first real decomposition:

- **`observed_minor` beside `committed_minor`.** They are not expected to agree, and the
  disagreement is the check. Measured: a power bill entered as "$250 fortnightly" is $541.67/mo
  against an observed $410, because not every fortnight landed a recorded payment. Over 110%
  produces a warning naming the category; it is reported rather than corrected, because the ledger
  may be incomplete or the cadence may be wrong and only the household knows which.
- **A residual under 5% of the observed level gets no volatility.** The measured figure is
  *relative*, so once the mean is a couple of dollars the ratio is a divide-by-small artifact that
  pins to the 300%/yr ceiling — and on a $2 residual that ceiling is a lognormal whose two-sigma
  tail is several hundred dollars, inventing spending out of the rounding on a bill the model
  already handles exactly.

A commitment nobody pays any more is worse than none, because it carries the false authority of a
stated amount: `stale_commitments` warns when an excluded merchant has been silent for three months.
Only checkable where a merchant is named, and the silence about the rest is honest rather than
reassuring.

An indexed rate does not decay. The `TREND_FULL_STRENGTH_MONTHS`/`TREND_HALF_LIFE_MONTHS` apparatus
exists to walk a rate fitted over a finite window back toward an anchor once the projection runs
past that window; an inflation assumption *is* the long-run rate, from month 1.

**Double counting income.** A stream already landing in the bank is *also* inside the fitted
baseline of the category it lands in. `income_streams.linked_category_id` is what prevents counting
it twice — the category's baseline becomes the residual, and `reconciliations` reports modelled
against recorded so a mistake is visible. Netting rather than excluding, because excluding would
silently drop the income the streams do not explain (interest, a gift, an unmodelled second job).

**Double counting loan interest.** The same defect on the other side of the ledger, and it cost a
year's interest twice over. A repayment leaves as two legs: principal, which moves cash into the
liability and nets out of net worth, and interest, which simply goes. `simulate` charges both to
cash through `Repayment::cash_out`. The interest leg is *also* a genuine expense row, and it is
normally recorded on the account the money came **from** — a revolving-credit facility, a chequing
account — not on the loan, so `is_excluded_from_spend` (which only excludes the loan's own rows)
does not reach it and it sits inside the fitted baseline of whatever category the household books
interest into. Both mechanisms then spend it.

The schedule wins and the fit defers, because the schedule is the better model of the same money on
every axis: it declines as the balance amortises where a baseline is flat, it stops when the loan is
repaid where a baseline runs to the horizon invoicing a mortgage that no longer exists, and it comes
from the contract rather than from however many months of ledger happen to exist.
`schedule_interest_by_category` nets it out, reading *which* category from the loan account's own
repayment rows — the household's own filing of that loan's servicing, not a guess. A loan whose own
rows are uncategorised is left alone and still double-counts: there is nothing in the data saying
where its interest went, and the category keeps reporting `derived` rather than
`modelled_from_schedule`, which is how you spot one.

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

## Streaming, and where the time actually goes

`GET /api/forecast/stream` is the same projection as `GET /api/forecast`, delivered as it firms
up: a `snapshot` event after 10 paths, then 100, then 1 000, then every 1 000 and once more at the
end, with counter-only `tick` events roughly every 1% of the run so a progress bar has something
to move with. Each snapshot is a *complete* projection over fewer paths — the same months, wider
bands — and reports `simulations: <paths so far>`, so a partial answer says how partial it is. The
final snapshot is byte-for-byte what the JSON route returns for the same query, which
`specs/forecast.spec.ts` asserts.

It exists because of the shape of the cost, measured against real data (25 accounts, 22 top-level
categories, 8 094 transactions, 2 543 valuations) on an M4:

| horizon | load phase | Monte Carlo | total | first snapshot |
| --- | --- | --- | --- | --- |
| 12 | 28 ms | 3 ms | 31 ms | ~28 ms |
| 360 | 80 ms | 75 ms | 154 ms | 66 ms |

Two things follow. The **load phase dominates a short horizon** — resolving assumptions is a dozen
SQLite queries and it does not get cheaper with fewer paths — so the floor on a first paint is the
loads, not the arithmetic. And the arithmetic is what a long horizon spends its time on, which is
what streaming hides: at 30 years the first honest picture of a *new* query arrives in 66 ms
instead of 341 ms, because ten paths is half a percent of the work.

Before this, the page held the **previous** run's numbers on screen for that whole time, with no
indication anything was happening — and fired two simulations per visit, because it had both an
`onMount` and an effect.

## Threading, and the one thing it restated

The path loop runs across threads. `sure_app::forecast::Parallelism` is injected, not sensed:
`sure-api`'s `compute` module owns the policy, because it holds the semaphore and is the only thing
that knows what else is running. It reserves one slot to be admitted (unchanged) and then as many
spare ones as it can take without dropping the pool below half — a forecast that grabbed every core
would shed the `/api/reports/*` calls the same dashboard load fires beside it, which is a worse
failure than a slower forecast.

What had to change to allow it: the loop used to advance **one** `StdRng` across every path in
sequence, so path *k* depended on how many draws paths `0..k` happened to take — and
`rand_distr`'s normal sampler is rejection-based, so that count is not even fixed. Each path now
seeds its own RNG from `(seed, path)`, exactly as events already did. **That restated every figure
once.** The distribution is unchanged, a seed still reproduces its run byte for byte, and it now
does so whatever the thread count and whatever `simulations` was asked for — path 7 is path 7 in a
10-path run and in a 5 000-path one, which it was not before. That last property is what makes a
streamed prefix *converge* on the full answer instead of being redrawn at each snapshot.

How much it moved, measured on the real household over five seeds at 2 000 paths (before against
after, same data, same day):

| figure | shift |
| --- | --- |
| +1 month median net worth | 0.00% |
| +13 month median | −0.00% |
| +13 month P10 | −0.13% |
| +13 month P90 | +0.04% |
| +13 month mean | +0.02% |

For scale: the seed-to-seed spread *within the old code* was up to 0.46% on the same figures. The
restatement is smaller than the noise the projection already had, which is the result to expect —
a different set of draws from the same distribution.

Merge order is not part of the answer, which is what made this safe: `band_from_samples` sorts
before it takes its mean as well as its percentiles, `negative_cash` is a count, and a milestone's
months are sorted too — so every output is a function of the sample *multiset*.

`every_way_of_running_a_simulation_agrees` pins all of it: one thread or nine, aggregating once or
at every checkpoint, the `ForecastResult` is identical.

## Operational notes

- `MAX_HORIZON_MONTHS` is 360, and paths trade against months under `MAX_PATH_MONTHS`, so a 30-year
  run costs about what a 5-year one does. `ForecastResult` echoes the `horizon_months` and
  `simulations` actually run — a caller asking for more can tell.
- `GET /api/forecast` is in `LONG_ROUTES`, and so is `/api/forecast/stream` — for the stream that is
  about the *head*, which waits on the loads; a streamed body is outside every deadline. See
  [HTTP.md](HTTP.md).
- `sure.forecast.simulate.duration` measures one run including its intermediate aggregations, so a
  streamed run reads slightly higher than a one-shot one of the same size. That is the honest
  reading of "how long did this take".
- The forecast no longer loads the whole `transactions`/`valuations` tables. It reads
  `ACCOUNT_TREND_MONTHS + 1` months plus the per-account seed, like every other report —
  `monthly_value_series` clamps its own lookback to that window anyway.
- Tax figures in `sure_core::tax` are dated and append-only. Editing an entry restates a tax year
  that has already happened; add a new scale instead, and record where the figures came from.
