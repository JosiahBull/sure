# Importing files

Bank feeds reach back about two years. A bank's own export reaches seven, and for a student loan
or a brokerage there is no feed of transactions at all — so uploading a file is how an account's
history gets past what syncs on its own. This is how that works.

One endpoint, one pipeline, one panel. It was four of each until 2026-08-06, and the four
disagreed about nearly everything a person would expect to be the same.

## What it looks like from outside

```
POST   /api/import ?dry_run= &opening_balance= &assign= &source=
DELETE /api/import/{account_id}/{source}
GET    /api/imports ?account_id=
```

The body is the file's bytes — a `.csv`, an `.xlsx`, a `.zip` of either, or a Sharesies export
zip. **Which importer it belongs to is decided from the bytes**, not from a filename, a content
type, or the URL. Nobody has to know which of four buttons their download needs.

In the UI it is one page, `#/settings/import`, and one component
([`ImportPanel.svelte`](../packages/web/src/lib/ImportPanel.svelte)) that the page and each
account row both render — the row passing an `accountId` so the upload is pre-scoped to it.
Everything that says "Import" goes to the same place.

### Preview, then commit

`?dry_run=true` runs everything except the write and reports what a commit would do. It is the
same code path up to one branch, placed after everything that could refuse or warn and before the
only write, so **a preview cannot describe an import that wouldn't happen**. The commit then sends
every account assignment back explicitly (`?assign=`), so what was on screen is what runs.

`import.spec.ts` asserts that as an equality — the preview's `would_import` against the commit's
`imported + skipped` — for every source. Not that a preview returns *something*: a preview that
describes a different import is worse than no preview.

### Undo is per source, not per upload

`DELETE /api/import/{account_id}/{source}` removes everything one importer put on one account,
leaving every other source's rows alone. It is deliberately not per-upload, and that isn't a
shortcut: overlapping uploads share their content-derived ids, so a second upload of an
overlapping window *skipped* its rows rather than writing them. There is nothing of it on its own
left to take back. "Undo the import I did on Tuesday" has no honest answer; "remove what myIR put
here" does.

`GET /api/imports` is the log — one row per (upload, account), the counterpart to `provider_syncs`
for uploads. It survives an undo, because the import did happen; the panel's heading says "Import
history" for that reason and not "Imported here".

## The pipeline

```
sniff → parse → route to an account → derive that account's cutover → hold back
      → reconcile → [dry run stops here] → write → record → report
```

Every stage is in [`sure_app::import`](../packages/app/src/import/mod.rs). A source's real
differences live in two places and nowhere else:

* **[`ImportAdapter`](../packages/app/src/ports.rs)** — how to recognise a blob and how to read
  it. Implemented in [`sure_providers::import`](../packages/providers/src/import.rs) over the
  parsers, which is the same inversion `TransactionProvider` uses: the adapters depend on the
  core, and the composition root injects a registry.
* **[`ImportSource`](../packages/core/src/import.rs)**'s own methods — which account kinds take
  this source, what may hold it back, whether a lone upload may route to the only candidate, and
  the provider tag its rows carry. Each is an exhaustive match, so a fifth importer is a compile
  error at every question it has to answer.

An adapter never answers a question about itself that its *output* can answer instead. A source
"supports" reconciliation by knowing its export's stated closing balance; it "supports" holdings
by returning `ParsedExtras::Brokerage`. There are no capability flags to keep in step.

### The cutover, and why it is never a parameter

Dedupe is `(provider, external_id)` and cannot see across two sources, so one movement arriving
from both a feed and an uploaded export is two rows in the ledger, permanently. The cutover is the
date from which a feed owns the account, and an import stops there.

It is read from the account's own feeds so the two halves can't be made to overlap by getting an
argument wrong:

1. A connection that derives transactions from a balance feed states where its half begins
   (`derive_from`). That needs no rows to exist yet, which is why it is consulted first.
2. Otherwise, the earliest date any *other* feed has posted for this account.
3. Otherwise — a feed is connected and enabled but has done neither — see the rule below.

That third case is the dangerous one, because "nothing else posts here" and "we couldn't tell"
would both produce a silent `None`, which imports the file whole. So
[`CutoverRule`](../packages/core/src/import.rs) makes each source say what to do:

| Rule | Sources | On a connected but silent feed |
| --- | --- | --- |
| `Strict` | ASB, plain CSV | **Block that item.** One re-upload against a doubled ledger nobody notices. |
| `Lenient` | myIR | Take it at face value: IR posts no transactions, so nothing is waiting. |
| `Never` | Sharesies | No cutover at all — nothing else posts a wallet, a lot, or a dividend. |

Blocking myIR would give advice ("sync it, then import again") that can never come true, which is
the whole reason the rule is a property of the source rather than one behaviour for all of them.

**A block is one item's, never the upload's.** It is a fact about one target account, and an upload
is routinely a zip bound for a dozen of them, so the item carries an
[`ImportBlock`](../packages/core/src/import.rs) — `reason`, the sentence to show, and the feeds by
id — with `would_import: 0` and nothing written, while every other item imports. It was an `Err`
out of the service until 2026-08-08, which made one pending feed fail the whole request: eleven
good imports thrown away to report a twelfth's problem, and the preview died with it, so the "skip
this one" control that would have resolved it never rendered.

Three ways out, all of them in the row that reported it:

* **Sync the feed** — then its posted rows establish the cutover the ordinary way. The ids in
  `feeds` are what let the panel offer this in place rather than sending the reader to another
  screen.
* **Disable it** — nothing will ever post, so there is no window to hold back.
* **State the date it owns from**, per item, as `?cutover=<source account>:<YYYY-MM-DD>`. Only the
  person importing can know when a feed that has never spoken will start. This is the *one* input
  to the cutover, and it stays consistent with the heading above by being consulted **only** where
  the derivation is blocked and the reason is one a date can answer
  (`ImportBlockReason::resolvable_by_stating_cutover` — an unreadable ledger date is not, because
  the read a date would be checked against is the broken thing). Where the feeds do establish a
  window, theirs wins and the item's warnings say the stated date went unused. So a caller cannot
  widen an import by passing a later date; they can only answer a question nothing else can.

An unreadable `posted_at` on the account blocks the same way (`unreadable_ledger_date`): it is
`MIN()` under SQLite's BINARY collation, where a non-ISO date sorts ahead of every ISO one, so that
single row is exactly the one that would have decided the window.

### Skipping one thing in an upload

`?assign=<source account>:skip` imports nothing of that item. It has to be a statement rather than
an omission: leaving an item out of `assign` only silences the top routing tier, and the five below
it go on to place the file anyway. The UI's "skip this one" *was* that omission until 2026-08-08 —
it zeroed the row on screen and imported it regardless.

### Routing an upload to an account

Nothing inside a bank export names a Sure account — the file knows `12-3456-0000123-50` and Sure
knows "Emergency Fund". [`routing.rs`](../packages/app/src/import/routing.rs) bridges that, in
order of how much each tier *proves*, and only an unambiguous answer counts:

1. **`assign`** — the request said so. Checked against every account before a byte is read, so
   naming one that doesn't exist is a 404 and naming a wrong-kind one is a 422. That is what the
   per-account routes used to give for free by having the account in their path; falling through
   to the tiers below would put rows somewhere the caller didn't ask for and report success.
2. **A previous import** of the same source account, recovered from the ids those imports wrote
   (`ImportAdapter::source_account_of`) — the durable memory that makes a re-upload route itself.
3. **The account's stored number**, then **its name** containing the distinctive tail.
4. **The only candidate**, for a source whose export cannot identify itself (a myIR SLS id, a
   Sharesies export that names nothing). *Not* for a bank export: that carries an account number
   the tiers above already tried, so if they found nothing the honest reading is "that account
   isn't in Sure yet", not "it must be the one that is".
5. **Whose the export says it is** — a myIR preamble carries a `Name:` beside the SLS id, and Sure
   already knows who owns which account. The household's name has to be found *inside* the
   export's, not equal it, because IR writes `Surname, Given Names` with a middle initial that a
   roster of "Ari" and "Sam" will never carry; single-letter tokens are dropped from both sides so
   an initial can't match on its own. One person and one of their accounts, or it declines. This
   is the only tier that separates a household's two student loans on a *first* import — the SLS
   id matches no Sure field, and there is no history to infer from yet — and it runs *before*
   content matching because it is still an identifier the source stated. It also vetoes tier 4:
   an export naming Sam must not land on the one loan there happens to be when that loan is Ari's.
6. **The transactions the account already holds** — matching the export's rows against them.
   Signed amounts and one day's tolerance, because a feed and the bank disagree about *when* far
   more than about what (exact dates matched 1 row in 161 across one account's overlapping year;
   one day's slack matched 161 of 161). At least 10 matched rows and 80% of the overlapping
   window, because a transfer pair seen from both sides scores a *perfect* 2 of 2 and on rate
   alone would file one account's history into another.

Content matching is last on purpose. Every tier above it is an identifier; matching rows is
inference, and a stored number that disagrees with the inference means something is wrong with the
number — which is a thing to say, not to quietly route around.

## The sources

| Source | File | Names an account? | Cutover | Reconciles | Extras |
| --- | --- | --- | --- | --- | --- |
| `asb_csv` | ASB "Export transactions" `.csv`, either shape, or a `.zip` spanning accounts | yes, its number | `Strict` | yes, stated closing balance | — |
| `myir_sls` | myIR "TAP SLS Transactions" `.xlsx`, or a `.zip` of them | an SLS id, and the borrower's name | `Lenient` | no | — |
| `sharesies_zip` | a Sharesies export `.zip` | no | `Never` | no | holdings, dividends |
| `csv_upload` | any `.csv` with `date` and `amount` | no | `Strict` | no | — |

**Detection order is load-bearing**: Sharesies → myIR → ASB → plain CSV. Every format here is, or
can be, a zip (an `.xlsx` *is* one), and a bare CSV reader would happily claim a bank export —
which would import it with no cutover, no opening balance and no account routing, and report
success. So each sniff looks for something only its own format has, and the general one is asked
last. `?source=` overrides the guess, and a name that isn't a source is refused rather than
quietly ignored: a caller naming one is overriding the sniff on purpose.

### Where each file comes from

* **ASB** — FastNet, open the account → *Export transactions*, CSV, `YYYY/MM/DD`, widest range it
  allows. One file per account; select them all at once.
* **myIR** — export *TAP SLS Transactions*. One export reaches back about two years, so a whole
  loan takes several, and they must be uploaded **together**: the cross-file checks (a gap in
  coverage, two exports disagreeing about a day) can only be made with every export in hand, and
  each catches something the database could not detect afterwards. See
  [STUDENT-LOAN.md](STUDENT-LOAN.md) for the five fatal refusals and the sign rule.
* **Sharesies** — request an export in account settings; upload the zip whole.

### Several files at once

The browser wraps them in a stored (uncompressed) zip —
[`web/src/lib/zip.ts`](../packages/web/src/lib/zip.ts) — rather than making N requests, because
one upload is the only shape whose cross-file checks can run. That deliberately changes nothing
server-side: the bytes are the same shape as a hand-made zip, so
[`zipfile.rs`](../packages/providers/src/zipfile.rs)'s entry and byte ceilings and every zip-bomb
test cover this path exactly as they cover that one.

A `.zip` among several files is refused rather than nested — a zip inside a zip is a hostile shape
the ASB parser declines — with a message saying to import the archive on its own.

## Opening balances

An imported history that starts from nothing is wrong: the balance reconstruction reads an account
as 0 before its earliest transaction, so it would appear out of thin air at whatever its first
day's movements leave behind. Where an export states a closing balance, the account's value
immediately before the first row can be worked back from it, and is recorded as a one-off — it
moves the account's value without being money earned or spent, so balances count it and income
reports don't.

Two details that are easy to get wrong, and are pinned by tests:

* **It is derived in the parser, not the pipeline.** It runs backwards over *every* row in the
  file including any the cutover later holds back — those movements happened, they are just
  already on the ledger from the feed — and by the time the pipeline has held rows back, they are
  gone.
* **It is withheld when the account already has rows from before that date**, with a warning
  saying so. It would not be an opening balance then; it would be a large invented movement in the
  middle of existing history.

## Limits

One ceiling for every source, so there is nothing for four of them to disagree about:
`MAX_IMPORT_BODY_BYTES` (50 MiB) on `POST /api/import`, refused by the server before the handler
or any parser sees a byte, plus `sure_core::MAX_UPLOAD_BYTES` as the pipeline's own pre-dispatch
check and `zipfile.rs`'s per-entry and whole-upload expansion budget. The HTTP cap bounds what
*arrives*; nothing about it bounds what an archive expands **to**, which is the whole of a zip
bomb. See [HTTP.md](HTTP.md) for the request-level table.

## Testing

Four tiers, and what belongs in each:

* **Beside each parser** (`sure_providers::{asb, myir, sharesies, csv}`) — reading the file. Text
  repair, date-order inference, sign flips, the cross-file invariants, every hostile archive. No
  proxy, no database.
* **`sure_providers::import`** — detection and its order, including that a bank export is not
  claimed by the plain CSV reader.
* **`sure_app::import`** — the pipeline's own decisions: the cutover rules, holding back, the
  routing tiers and their thresholds. Pure functions over fakes.
* **`packages/api-tests`** — `import.spec.ts` for the shared spine (detection end to end, the
  source override, preview-equals-commit and undo *per source*, a browser-shaped multi-file zip,
  the shared ceiling, the log), and `asb/student-loan/brokerage.spec.ts` for what each source
  makes of its own file.

Two notes for anyone adding to these. The malformed-input specs state `?source=` explicitly,
because a malformed file is exactly what detection can't be trusted to place, and the point of
those tests is the parser's refusal. And a Sharesies import spawns a valuation backfill that
reaches the price feed, so a spec that uploads one must stub it — an unanswered upstream sends the
code under test down an error path nobody asked for (see `failOnUnstubbedRequests` in
`packages/api-tests/fixtures.ts`).

## Adding a source

1. A variant on `ImportSource` in [`sure-core`](../packages/core/src/import.rs). The compiler now
   lists every question it has to answer: its wire name, its label, its tag stem, which account
   kinds accept it, its cutover rule, whether a lone upload may route to the only candidate.
2. An `impl ImportAdapter` in [`sure-providers`](../packages/providers/src/import.rs) — `sniff`,
   `parse`, `source_account_of` — and a line in `ImportRegistry::new`, **in the right place**:
   more specific than the plain CSV reader.
3. Nothing in `sure-api` or `packages/web` changes. If either does, the seam is in the wrong
   place.

One warning about step 1. `ImportSource::tag_stem` returns strings that are **already written into
`transactions.provider`** in every existing database, and nothing migrates them. Changing one
doesn't rename anything: it orphans every row already imported under the old spelling from both
undo and the previous-import routing tier, silently, while the import still reports success.
`provider_tag_stems_are_the_ones_already_in_the_database` exists to make that a failing test
rather than a support question. New stems must also differ from every provider *kind*, because a
scheduled sync tags its rows `{kind}#{provider id}` from a different id sequence — which is why an
uploaded CSV is `csv-upload`, not `csv`.
