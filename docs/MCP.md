# MCP

Sure speaks the [Model Context Protocol](https://modelcontextprotocol.io), so an agent can
read the ledger and — under a second, separate opt-in — write to it.

> **Turning this on sends your financial data to a model provider.** Transaction
> descriptions and notes carry account numbers, IRD numbers, payee names and card
> last-fours, and everything a tool returns goes to whichever model the connecting client
> runs. That is a real departure from the rest of this app, whose whole premise is that your
> money data stays on your hardware. Nothing here is redacted: a memo is often the only way
> to identify a transaction, and a masked one would be useless for the job. This is why the
> feature is **off by default**, why it takes two switches to turn on (see below), and why
> writes are a separate step again — enabling it should be a decision, not something that
> happened.

## Turning it on

Two switches, and **both** have to allow it:

1. **`SURE_MCP`** — an environment variable, and a *ceiling*. It says the most this process
   will ever serve. Unset (the default) means `off`, and `/mcp` is not a route at all — a
   client's POST gets a 404, or a 405 from the static handler when the SPA is being served
   (`WEB_DIR`, as in the container image). Either way there is no MCP behind it.
2. **Settings → Preferences → Agent access** — the working mode, stored in the database and
   changeable from the app. What is served is this **clamped to the ceiling**.

```bash
SURE_MCP=read ./target/release/sure-api      # the app may choose off or read
SURE_MCP=write ./target/release/sure-api     # the app may choose off, read or write
./target/release/sure-api                    # off; the settings control is disabled
```

| Var | Default | Meaning |
| --- | --- | --- |
| `SURE_MCP` | `off` | `off` \| `read` \| `write` — the ceiling. Off means `/mcp` is not mounted. |
| `SURE_MCP_MAX_ROWS` | `200` | Ceiling on rows any one tool returns. |

The reason it is a ceiling and not just a default: the API has no authentication, so a
setting alone would mean anything that can reach the app can turn on agent access. Requiring
the environment first keeps "may this deployment do this at all" a question answered by
whoever runs the host. Setting `SURE_MCP=off` (or leaving it unset) is a guarantee the app
cannot override.

The app can only ever choose *down*. Asking for more than the ceiling is refused with a 422
naming the limit — not silently clamped, because a settings page showing `write` while the
server serves `read` is worse than an error.

**Changes apply immediately.** The served mode is resolved per request (one indexed
single-row read), so switching agent access off takes effect on the very next call, with no
restart and no cache to go stale. Off means *nothing* is served — not read-only — so an
assistant mid-conversation finds every tool gone. The server's instructions tell clients the
tool list can change under them and to re-list rather than assume a bug.

An unrecognised `SURE_MCP` value **stops the server** rather than falling back. Every other
tunable in `sure-server` warns and takes its default on a typo; this one cannot, because both
directions of the guess are wrong: `SURE_MCP=wrtie` silently serving nothing is a confusing
afternoon, and a value that fell back the other way would be an agent with write access to
the household ledger that nobody asked for.

Then point a client at it:

```bash
claude mcp add --transport http sure http://127.0.0.1:8080/mcp
```

`npx @modelcontextprotocol/inspector` works too, for poking at it by hand.

### Reaching it from anywhere but this machine

The endpoint accepts `Host` headers for loopback plus whatever is in `CORS_ALLOWED_ORIGINS`,
and refuses everything else. That is not paranoia about your network: a locally-running MCP
server is a DNS-rebinding target, where a page you visit resolves *its own* hostname to
`127.0.0.1` and POSTs to this endpoint from inside your browser. Serving Sure on a real
hostname means setting `CORS_ALLOWED_ORIGINS` — which you already have to do for the SPA — and
the MCP allowlist follows from it, so the two answers to "who may reach this process" cannot
drift apart.

Note there is still **no authentication**, here or on `/api` — that is a property of the whole
app (see `packages/api/src/security.rs`). MCP adds no new authorization hole; it adds a
convenient surface to an existing one. Bind to loopback, or put something in front.

## The tools

`packages/mcp/tool-manifest.json` is the committed contract — names, descriptions, arguments,
annotations, and which tier each is in. It is asserted against the running code by a snapshot
test, so it cannot quietly drift.

**Read tier** (served when the effective mode is `read` or `write`): `list_accounts`, `list_categories`, `list_merchants`,
`search_transactions`, `get_transaction`, `summarize_spending`, `net_worth`, `money_flow`,
`account_detail`, `preview_rule`, `list_rules`.

**Write tier** (added when the effective mode is `write`): `update_transaction`, `bulk_categorize`,
`create_transaction`, `record_valuation`, `create_merchant`, `create_category`, `save_rule`,
`run_rule`, `undo_rule_run`.

Two of these carry most of the design:

**`summarize_spending`** exists so nothing ever pulls four thousand rows to add them up. It
groups by category, merchant, account or month, totals server-side, and normalises currencies
— which a model doing the arithmetic itself gets silently wrong the moment two currencies are
involved. Every tool description and both workflow prompts point at it.

**`bulk_categorize`** is the only tool that can touch many rows at once, and it will not write
until it has told you how many rows it found and been told the same number back:

```
→ bulk_categorize { search: "z energy", category_id: 12 }
← Dry run: 34 transaction(s) would be changed.
  To apply, call again with dry_run=false and expect_count=34.

→ bulk_categorize { search: "z energy", category_id: 12, dry_run: false, expect_count: 34 }
← Updated 34 transaction(s).
```

An `expect_count` that no longer matches is refused. The point is not ceremony — it is that an
over-broad filter is the most likely mistake here and the one whose blast radius is the whole
ledger, and that a confirmation carrying the *number* cannot be satisfied by flipping a
boolean without having read the count.

### Not exposed, at any tier

- **Config import/export.** One replaces the entire database; the other dumps it into the
  model's context.
- **Deleting anything** — transactions, accounts, categories, rules.
- **Account creation and editing.** Configuration, and a mis-set `kind` changes how every
  figure about that account is computed.
- **Provider linking and discovery.** The joint-account rules in `SyncService::survey_accounts`
  exist so one bank account is not counted twice in net worth; that is not a judgement to hand
  to an agent.
- **File import.** Needs an upload; no tool shape for it yet.

The rules engine is the intended path for anything sweeping. It previews before it saves,
records every change in an audit log, and `undo_rule_run` puts a run back exactly.

## Conventions on the wire

Deliberate choices, each because the alternative fails silently:

- **Money is a decimal string with its currency** (`"-42.50"`, `"NZD"`), never the minor units
  it is stored in. A model handed `-4250` reports "$4,250" — not an error anything catches,
  just a confident wrong answer. Amounts going the other way are parsed once, at this edge,
  and an over-precise one (`10.005` in a 2-decimal currency) is refused rather than rounded.
- **Named date ranges** — `last_month`, `last_90_days`, `ytd`, `last_12_months`, `all_time` —
  because models do date arithmetic badly, most reliably around month ends. `last_month` is
  the previous *calendar* month.
- **Lists are pipe tables**, roughly a quarter the tokens of the equivalent JSON, and read as
  a table rather than as something to quote back field by field.
- **A capped list says so**, and points at `summarize_spending` rather than just offering the
  next page.
- **`unconverted` is shouted, not footnoted.** Where no exchange rate links a currency to the
  report currency, those transactions are excluded rather than counted at parity, and the
  result carries an `INCOMPLETE:` line naming them. A total missing a currency reads as
  complete otherwise.

`sure://conventions` is a resource carrying all of this, for clients that can attach a
resource to a conversation instead of spending a turn on it. There are three more —
`sure://accounts`, `sure://categories`, `sure://merchants` — and three prompts:
`monthly_review`, `tidy_uncategorised`, `explain_account`.

## How it fits

`packages/mcp` (`sure-mcp`) is a **second driving adapter**, sibling to `sure-api`:

```
sure-core ← sure-app ← sure-dal / sure-providers
               ↑   ↑
        sure-api   sure-mcp      ← neither knows the other
               ↑   ↑
             sure-server         ← builds one SqliteStore, injects both
```

It depends on `sure-app` and names no `sqlx`, no `sure_dal`, and no `sure_api` — the same
rules `sure-api` lives by. It declares its own `McpState`, a strict subset of the HTTP
adapter's: no import pipeline, no provider registry, no snapshot repo, because no tool may
reach them. That makes the "not exposed" list above a matter of what is in scope rather than
a matter of discipline.

The transport is Streamable HTTP in **stateless JSON-response mode**, mounted at `/mcp` in the
router `sure_api::build_app` already assembles (via its `extra: Router` parameter) rather than
on a listener of its own. So every MCP request gets the panic catching, request id, tracing,
rate limiting and body cap the API routes get, and one process remains the only writer to
`data/sure.db`. Stateless is what keeps it compatible with that stack: a held-open SSE stream
would sit against the 30-second request deadline and would still be open when the shutdown
drain came for it. In-flight calls take a child cancellation token from `sure-appbase`, so a
tool call is part of the drain rather than something the process walks away from mid-write.

## Testing

- `packages/mcp` inline tests — money rendering, window resolution, table escaping, the error
  mapping's scrubbing, and the tier split.
- `packages/mcp/tool-manifest.json` — the committed contract. Regenerate deliberately with
  `cargo test -p sure-mcp -- --ignored update_the_tool_manifest` and read the diff.
- `packages/api-tests/specs/mcp.spec.ts` — the real binary, driven as raw JSON-RPC over HTTP:
  that `/mcp` is absent when the ceiling is off, that a bad flag value stops startup, that the
  stored setting takes effect without a restart (off → read → write → off, checking the tool
  list each time), that the ceiling refuses a setting above it, that changing the base
  currency leaves agent access alone, and the whole `bulk_categorize` confirmation sequence.
