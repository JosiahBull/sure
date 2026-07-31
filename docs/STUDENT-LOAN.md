# Student loan: importing and tracking

An IR student loan is a liability Akahu can see the **balance** of but not the
**transactions** — the account attribute simply isn't there. Left alone, the account carries
an accurate balance and an empty ledger: net worth is right, but nothing shows what moved.

Its history therefore comes from two sources, joined at a cutover date:

| | Covers | Source | `transactions.provider` |
|---|---|---|---|
| **Backfill** | up to the cutover | myIR exports, uploaded on the account | `myir#<account id>` |
| **Forward** | the cutover onward | `balance_delta`, differencing the daily balance feed | `balance-delta#<provider id>` |

Neither determines the balance. `account_value_at` (`packages/app/src/reports.rs`) reads the
*valuations*, which the provider poll refreshes several times a day; the ledger only explains
them. That is what makes the forward half safe to run unattended — a missed or duplicated
derived row is cosmetic, never a wrong net worth.

## Backfilling from myIR

myIR caps a single export at about two years, so reaching a loan's origination takes several
downloads whose windows overlap. Upload them **together**, as a zip: the cross-file checks
can only be made while every export is in hand, and each catches something the database
could not detect afterwards.

1. In myIR, open the student loan account → *Transactions* → export to Excel. Pick the
   widest window it will give you.
2. Drop the `.xlsx` in `myir-export/` (gitignored — it is real transaction history, same as
   `sharesies-export/`). Name it for its window, e.g. `sls_2019-07-31_2026-07-31.xlsx`, and
   never delete or overwrite one: the directory *is* the manifest.
3. Settings → Accounts → the loan → **Import** → **Import myIR export**. Upload the single
   `.xlsx`, or a `.zip` of the whole directory once you have more than one.

The response reports how many rows were new, how many were already there, the SLS account
the exports were for, and the window they cover. Re-uploading is free and expected — the
import dedupes on a content-derived id, so overlapping windows cost nothing.

### What the parser refuses

All fatal, all in `sure_providers::myir`, all describing a failure that would otherwise be
invisible once the rows were in:

- **Exports for different SLS accounts.** A different suffix is a different product.
- **A row outside its own export's window** — the window metadata is what the other checks
  rest on.
- **A gap in coverage.** The dangerous one: a missing window looks exactly like a quiet
  period with no activity, and the balance reconstruction absorbs it silently.
- **Overlapping exports that disagree about a day.** Each export is authoritative for every
  day it covers, so a disagreement means IR restated something. The import is
  `INSERT OR IGNORE` with no update-on-conflict, so a restated row would land *beside* the
  stale one and double-count. To resolve: keep the rows from the export with the newest
  "as at", delete the superseded transaction, re-upload.
- **A non-student-loan account type in the sheet**, which means the export came from a
  different myIR view.

A transaction type the parser hasn't seen is **not** refused — the sign rule is uniform, so
it still lands the right way round — but it is returned as a warning so it gets a glance.

### The sign

IR writes its ledger with a debt increase positive. Sure stores a liability's balance
negative, so a repayment — which *reduces* the debt — has to be a positive transaction. Every
row is negated on import, with no per-type special-casing.

The check that this is right: with a ledger complete from origination, the sum of the
imported rows equals the balance the provider independently reports, to the cent.

## The forward half

`sure_app::tasks::balance_delta` runs daily, differences consecutive `source='provider'`
valuations, and imports each non-zero change. It is **opt-in per connection**, because a
provider that does return real transactions (Akahu for a mortgage or an everyday account)
would otherwise have every movement counted twice. Set on the connection's `config`:

```json
{ "external_account_id": "acc_…",
  "derive_transactions_from_balance": true,
  "derive_from": "2026-07-31" }
```

`derive_from` is the cutover, and the import reads it back: rows on or after it are held
back, so the two halves cannot drift into each other. There is no UI for editing a
connection's config — `PUT /api/providers/{id}` takes the whole `SaveProvider` body.

Two details worth knowing:

- Only **closed** days are derived. `upsert_from_provider` rewrites today's valuation on
  every poll, so a delta computed against today would be stale the moment the next poll
  landed, and `INSERT OR IGNORE` would never correct it. Costs a day of lag.
- Movements on the same day net into one row. Balance-accurate, less granular.

## Reports

`is_excluded_from_spend` covers `student_loan` alongside `mortgage` and `brokerage`. Without
it, every repayment — positive, on a liability — would read as household income. It cannot be
shown as an expense either: that needs the opposite sign, which is what the balance
reconstruction depends on.

This assumes salary reaches the bank feed as **net** pay, so a PAYE repayment is already
absent from both income and expense. If income were ever tracked gross, revisit.

### Transfer links

`transfer_link` auto-pairs transactions with exact opposite amounts within ±5 days. For a
voluntary payment from a tracked bank account that is exactly right — the bank leg stops
looking like an expense. A PAYE deduction has no bank leg, so any link on one is a false
positive, and a false link silently drops the counterpart bank transaction from the spend
reports. Worth checking after a large import:

```sql
SELECT t.posted_at, t.amount_minor, t.description, o.account_id, o.description
FROM transactions t JOIN transactions o ON o.id = t.linked_transaction_id
WHERE t.account_id = <id>;
```

Unlink a wrong pair with `DELETE /api/transactions/{id}/link`.

## Traps

- **Don't add a manual valuation to an Akahu-fed loan.** The unique index only constrains
  `source='provider'`, so a manual row on a day the provider also synced creates a *second*
  valuation for that day — and the report loader has no `ORDER BY`, so which one wins net
  worth is unspecified.
- **Don't set `term_months` in the account's loan metadata.** `subtype`, `lender`,
  `original_amount_minor` and `interest_rate_bps` are already required, so two of the four
  fields `amortization_terms` looks for are present. Add `term_months` and `start_date` too
  and the forecast switches the account to a deterministic amortisation schedule, discarding
  the real balance for a fabricated straight line. A student loan has no term.
- **Don't model this with the `crons` feature.** It is monthly-only, never runs in the
  background, and writes transactions with `provider`/`external_id` left NULL — so they
  can't be deduped, can't be told apart from manual rows, and are orphaned untraceably if
  the cron is deleted.

## Rollback

`transactions` has no foreign key to `providers`, so deleting a connection leaves its rows.
Each half resets independently, and rebuilds by re-running:

```sql
DELETE FROM transactions WHERE provider = 'myir#<account id>';
DELETE FROM transactions WHERE provider = 'balance-delta#<provider id>';
```
