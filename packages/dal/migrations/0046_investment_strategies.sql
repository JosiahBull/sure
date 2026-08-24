-- What the household does with money it does not spend, over a stated window.
--
-- Everything else in the forecast models what *arrives* and what *leaves*. Nothing modelled the
-- decision in between, so surplus cash simply accumulated in the pool at no return — which
-- understates a household that saves and badly misrepresents one that is about to receive a lump
-- sum. A strategy is that decision, written down: a share of income, a share of any windfall, debt
-- above a rate paid off first, and the remainder invested somewhere.
--
-- **Windowed rather than singular**, and that is the point of the table. "80% of one salary until
-- March 2028, then 50%" is two strategies that tile time, not one strategy with a schedule inside
-- it — and writing it the second way would mean inventing a second calendar next to the one
-- `forecast_events` already owns. Overlaps are allowed: two strategies both active in a month both
-- sweep, because a household can be saving a share of a salary *and* directing a windfall.
--
-- `target_account_id` is where the money goes, and the account's **own** growth assumption governs
-- the return. No `assumed_return_bps` column, deliberately: that would be a second source of truth
-- for one account's rate, and the first — `forecast_assumptions` — is already the place the user
-- edits it and the Assumptions tab already shows it. "Invest in the S&P at 10%" is therefore an
-- account with a +10%/yr override, which also makes the assumption visible and arguable instead of
-- buried in a strategy row.
--
-- `debt_above_bps` is matched against a liability's *recorded* annual rate. A liability with no rate
-- on record is not paid down and the projection says so, because paying off a debt on the strength
-- of a rate threshold when the rate is unknown is a guess dressed as a policy.
CREATE TABLE investment_strategies (
    id                   INTEGER PRIMARY KEY,
    label                TEXT NOT NULL,
    -- Inclusive. NULL `active_to` is open-ended, matching `income_streams.ends_on`.
    active_from          TEXT NOT NULL,
    active_to            TEXT,
    -- Share of contributing income swept, basis points. Bounded at 100%: a household cannot invest
    -- more of a salary than the salary.
    income_share_bps     INTEGER NOT NULL DEFAULT 0
                             CHECK (income_share_bps BETWEEN 0 AND 10000),
    -- One stream, or NULL for every stream the household has. RESTRICT rather than SET NULL:
    -- silently widening "80% of one salary" to "80% of all income" when that stream is deleted is a
    -- change of meaning with no trace, which is what `income_streams.linked_category_id` already
    -- refuses for the same reason.
    income_stream_id     INTEGER REFERENCES income_streams(id) ON DELETE RESTRICT,
    -- Share of any cash raised by a `liquidate` effect in the same month.
    windfall_share_bps   INTEGER NOT NULL DEFAULT 10000
                             CHECK (windfall_share_bps BETWEEN 0 AND 10000),
    -- Pay down any liability whose recorded annual rate exceeds this, highest rate first, before
    -- investing anything. NULL means "do not touch debt".
    debt_above_bps       INTEGER CHECK (debt_above_bps IS NULL OR debt_above_bps >= 0),
    -- Where the remainder goes. RESTRICT for the same reason as the stream.
    target_account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
    enabled              INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    sort_order           INTEGER NOT NULL DEFAULT 0,
    notes                TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- A window that ends before it starts is never active, which the projection would treat as a
    -- strategy that does nothing. The caller meant something else.
    CHECK (active_to IS NULL OR active_to >= active_from)
) STRICT;
CREATE INDEX idx_investment_strategies_target ON investment_strategies(target_account_id);
CREATE INDEX idx_investment_strategies_stream ON investment_strategies(income_stream_id);
