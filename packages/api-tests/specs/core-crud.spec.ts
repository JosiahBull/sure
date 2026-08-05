import { test, expect } from "../fixtures";
import {
  createAccount,
  createCategory,
  createMerchant,
  createTransaction,
  getTransaction,
} from "../helpers";

test("currencies are seeded", async ({ api }) => {
  const { data } = await api.GET("/api/currencies", {});
  const codes = (data ?? []).map((c) => c.code);
  expect(codes).toContain("NZD");
  expect(codes).toContain("USD");
});

test("settings default to NZD and can be updated", async ({ api }) => {
  const { data } = await api.GET("/api/settings", {});
  expect(data?.base_currency_code).toBe("NZD");

  const updated = await api.PUT("/api/settings", { body: { base_currency_code: "usd" } });
  expect(updated.response.status).toBe(200);
  expect(updated.data?.base_currency_code).toBe("USD");

  const bad = await api.PUT("/api/settings", { body: { base_currency_code: "ZZZ" } });
  expect(bad.response.status).toBe(422);
});

test("account lifecycle and classes", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const fetched = await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  expect(fetched.data?.kind).toBe("bank");
  expect(fetched.data?.class).toBe("cash");

  const shares = await createAccount(api, "Startco Options", "shares_private", "USD");
  const sharesBody = await api.GET("/api/accounts/{id}", { params: { path: { id: shares.id } } });
  expect(sharesBody.data?.class).toBe("investment");

  const bad = await api.POST("/api/accounts", {
    body: {
      name: "Bad",
      kind: "bank",
      currency_code: "ZZZ",
      archived: false,
      sort_order: 0,
      ownership: { kind: "joint" },
    },
  });
  expect(bad.response.status).toBe(422);

  const del = await api.DELETE("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  expect(del.response.status).toBe(204);
  const gone = await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  expect(gone.response.status).toBe(404);
});

test("categories nest and cycles are rejected", async ({ api }) => {
  const parent = await createCategory(api, "Housing");
  const child = await createCategory(api, "Mortgage", "expense", parent.id);
  const grandchild = await createCategory(api, "Interest", "expense", child.id);

  const { data: tree } = await api.GET("/api/categories/tree", {});
  const housing = (tree ?? []).find((c) => c.category.id === parent.id)!;
  expect(housing.children[0].category.id).toBe(child.id);
  expect(housing.children[0].children[0].category.id).toBe(grandchild.id);

  // Nesting Housing under its own grandchild is a cycle.
  const cycle = await api.PUT("/api/categories/{id}", {
    params: { path: { id: parent.id } },
    body: { name: "Housing", kind: "expense", parent_id: grandchild.id, sort_order: 0 },
  });
  expect(cycle.response.status).toBe(422);
});

test("categories nest at most three levels deep", async ({ api }) => {
  const housing = await createCategory(api, "Housing");
  const utilities = await createCategory(api, "Utilities", "expense", housing.id);
  const power = await createCategory(api, "Power", "expense", utilities.id);

  // A fourth level has no column to be drawn in, so it's refused at the source.
  const tooDeep = await api.POST("/api/categories", {
    body: { name: "Off-peak", kind: "expense", parent_id: power.id, sort_order: 0 },
  });
  expect(tooDeep.response.status).toBe(422);

  // Re-parenting is the subtler half: Utilities is itself only one level down, but it
  // brings Power with it, so landing it under a depth-1 category would push Power to four.
  const food = await createCategory(api, "Food");
  const dining = await createCategory(api, "Dining", "expense", food.id);
  const movedSubtree = await api.PUT("/api/categories/{id}", {
    params: { path: { id: utilities.id } },
    body: { name: "Utilities", kind: "expense", parent_id: dining.id, sort_order: 0 },
  });
  expect(movedSubtree.response.status).toBe(422);

  // The leaf alone still fits there — it's the subtree that didn't.
  const movedLeaf = await api.PUT("/api/categories/{id}", {
    params: { path: { id: power.id } },
    body: { name: "Power", kind: "expense", parent_id: dining.id, sort_order: 0 },
  });
  expect(movedLeaf.response.status).toBe(200);
});

test("transaction filters and the one-off toggle", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const groceries = await createCategory(api, "Groceries");

  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-10", amount_minor: -5000, category_id: groceries.id });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -8000, category_id: groceries.id });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-20", amount_minor: -100000, is_one_off: true });

  const feb = await api.GET("/api/transactions", { params: { query: { from: "2026-02-01", to: "2026-02-28" } } });
  expect(feb.data?.length).toBe(2);

  const grocery = await api.GET("/api/transactions", { params: { query: { category_id: groceries.id } } });
  expect(grocery.data?.length).toBe(2);

  const withoutOneOff = await api.GET("/api/transactions", { params: { query: { include_one_off: false } } });
  expect(withoutOneOff.data?.length).toBe(2);
  const all = await api.GET("/api/transactions", {});
  expect(all.data?.length).toBe(3);
});

test("bulk update patches, clears, and leaves untouched fields alone", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const groceries = await createCategory(api, "Groceries");
  const merchant = await createMerchant(api, "Countdown");

  const a = await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-01", amount_minor: -100 });
  const b = await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-02", amount_minor: -200 });
  const c = await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-03", amount_minor: -300 });

  // Set category + merchant + one-off on a and b; c is left out and must not change.
  const patch = await api.POST("/api/transactions/bulk-update", {
    body: { ids: [a.id, b.id], category_id: groceries.id, merchant_id: merchant.id, is_one_off: true },
  });
  expect(patch.response.status).toBe(200);
  expect(patch.data?.affected).toBe(2);
  for (const id of [a.id, b.id]) {
    const t = await getTransaction(api, id);
    expect(t.category_id).toBe(groceries.id);
    expect(t.merchant_id).toBe(merchant.id);
    expect(t.is_one_off).toBe(true);
  }
  const untouched = await getTransaction(api, c.id);
  expect(untouched.category_id).toBeNull();
  expect(untouched.is_one_off).toBe(false);

  // An explicit null clears the category; omitting merchant leaves it as-is.
  const cleared = await api.POST("/api/transactions/bulk-update", {
    body: { ids: [a.id], category_id: null },
  });
  expect(cleared.data?.affected).toBe(1);
  const afterClear = await getTransaction(api, a.id);
  expect(afterClear.category_id).toBeNull();
  expect(afterClear.merchant_id).toBe(merchant.id);

  // A non-existent category is rejected.
  const bad = await api.POST("/api/transactions/bulk-update", {
    body: { ids: [a.id], category_id: 999999 },
  });
  expect(bad.response.status).toBe(422);
});

test("bulk delete removes rows and unlinks the other side of a transfer", async ({ api }) => {
  const checking = await createAccount(api, "Checking", "bank");
  const savings = await createAccount(api, "Savings", "savings");
  const solo = await createTransaction(api, { account_id: checking.id, posted_at: "2026-01-01", amount_minor: -50 });

  const { data: pair } = await api.POST("/api/transfers", {
    body: {
      from_account_id: checking.id,
      to_account_id: savings.id,
      posted_at: "2026-03-01",
      from_amount_minor: 25000,
      description: "Move to savings",
    },
  });
  const [out, inflow] = pair!;

  // Delete the solo row and the outflow side of the transfer in one call.
  const del = await api.POST("/api/transactions/bulk-delete", { body: { ids: [solo.id, out.id] } });
  expect(del.response.status).toBe(200);
  expect(del.data?.affected).toBe(2);

  expect((await api.GET("/api/transactions/{id}", { params: { path: { id: solo.id } } })).response.status).toBe(404);
  expect((await api.GET("/api/transactions/{id}", { params: { path: { id: out.id } } })).response.status).toBe(404);
  // The surviving side of the transfer had its link cleared by the FK cascade.
  expect((await getTransaction(api, inflow.id)).linked_transaction_id).toBeNull();
});

test("a transfer creates a reciprocally-linked pair", async ({ api }) => {
  const checking = await createAccount(api, "Checking", "bank");
  const savings = await createAccount(api, "Savings", "savings");

  const { data: pair, response } = await api.POST("/api/transfers", {
    body: {
      from_account_id: checking.id,
      to_account_id: savings.id,
      posted_at: "2026-03-01",
      from_amount_minor: 25000,
      description: "Move to savings",
    },
  });
  expect(response.status).toBe(201);
  const [out, inflow] = pair!;
  expect(out.amount_minor).toBe(-25000);
  expect(inflow.amount_minor).toBe(25000);
  expect(out.linked_transaction_id).toBe(inflow.id);
  expect(inflow.linked_transaction_id).toBe(out.id);

  const unlinked = await api.DELETE("/api/transactions/{id}/link", { params: { path: { id: out.id } } });
  expect(unlinked.data?.linked_transaction_id).toBeNull();
  expect((await getTransaction(api, out.id)).linked_transaction_id).toBeNull();
});

// ---- dates the reports can actually read ------------------------------------------------
//
// Every date column is a bare `TEXT` whose only contract used to be a comment, and every
// reader parses it back as `%Y-%m-%d`. So a `31/07/2026` was a **201**: it came back from
// `GET /api/transactions`, it rendered in the ledger, and it was invisible to the balance
// sheet, net worth, category breakdown, money-flow graph and forecast — each of which drops a
// row whose date won't parse. `date('31/07/2026')` is NULL too, so `?from=`/`?to=` hid it,
// which is how it stayed hidden. No spec in this suite had ever sent a malformed date; that
// absence is why it survived. `sure_core::IsoDate` now parses these at the wire edge.

/** Shapes that must never reach a date column, and why each one is not merely pedantic. */
const REJECTED_DATES = [
  "31/07/2026", // day-first — the original bug
  "2026-7-1", // unpadded: `chrono` accepts it, but it sorts after "2026-12-01" as text
  "", // blank
  "   ", // blank once trimmed
  "not a date",
  "2026-07-31T09:30:00Z", // a provider payload's datetime, silently truncated before
  "2026-07-31 09:30:00",
  "2026-02-30", // shaped right, no such day
  "2026-13-01",
  "1000-01-01", // outside the plausible window: a millennium-wide chart axis
  "2500-01-01",
] as const;

test("a transaction date the reports cannot read is refused, not stored", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");

  for (const posted_at of REJECTED_DATES) {
    const { response, error } = await api.POST("/api/transactions", {
      body: { account_id: acc.id, posted_at, amount_minor: -1_000, description: "bad date" },
    });
    // 422, not the 201 this used to be.
    expect(response.status, posted_at).toBe(422);
    expect(error?.error.code, posted_at).toBe("validation");
    // And the message names the field. This used to be the framework's generic
    // "Unprocessable Entity." — axum renders a body rejection as text/plain, so
    // `request_context` re-clothed it and serde's text was lost. `crate::extract::Json`
    // answers the rejection itself now, so a caller can tell *which* field it got wrong.
    expect(error?.error.message, posted_at).toContain("posted_at");
    expect(error?.error.message, posted_at).not.toBe("Unprocessable Entity.");
  }

  // And nothing was written — a refusal that still inserted would be the same bug wearing a
  // different status code.
  const list = await api.GET("/api/transactions", {});
  expect((list.data ?? []).some((t) => t.description === "bad date")).toBe(false);
});

test("a bad date is refused on every write that carries one", async ({ api }) => {
  const house = await createAccount(api, "House", "real_estate");
  const checking = await createAccount(api, "Checking", "bank");
  const savings = await createAccount(api, "Savings", "savings");

  const valuation = await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: house.id } },
    // A bad `as_of` is the worst of the family: `account_value_at` anchors on valuations, so
    // the property's value silently reverts to whatever came before.
    body: { as_of: "31/07/2026", value_minor: 850_000_00 },
  });
  expect(valuation.response.status).toBe(422);
  // Nothing landed: the helper's zero opening balance seeds no ledger row, so this account's
  // valuations are exactly the ones a spec creates.
  expect((await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: house.id } } })).data)
    .toHaveLength(0);

  const transfer = await api.POST("/api/transfers", {
    body: {
      from_account_id: checking.id,
      to_account_id: savings.id,
      posted_at: "31/07/2026",
      from_amount_minor: 25_000,
      description: "Move to savings",
    },
  });
  expect(transfer.response.status).toBe(422);

  const cron = await api.POST("/api/crons", {
    body: { name: "Appreciation", account_id: house.id, kind: "appreciation", rate_bps: 300, start_date: "31/07/2026" },
  });
  expect(cron.response.status).toBe(422);

  // `expected_on`, on the event itself: the date an effect is applied on is the event's, so this
  // is the only date this write carries. The body is the current `SaveForecastEvent` shape — it
  // used to be a flat `{target_type, target_id, kind, effective_date, amount_minor}`, which the
  // life-events rework replaced wholesale, and a body serde cannot deserialise is *also* a 422.
  // So this assertion went on passing while testing nothing about dates at all, and `tsc` was the
  // only thing that could still see the difference (`pnpm test:api:check`).
  const event = await api.POST("/api/forecast/events", {
    body: {
      label: "Roof",
      kind: "adjustment",
      expected_on: "31/07/2026",
      effects: [
        {
          kind: "one_off_amount",
          target: { kind: "account", account_id: house.id },
          amount_minor: -5_000_00,
        },
      ],
    },
  });
  expect(event.response.status).toBe(422);
  expect(event.error?.error.message, "the refusal has to name the date field").toContain(
    "expected_on",
  );
});

// The actual defect was never "a bad date exists" — it was that the ledger and the balance
// disagreed permanently with no error anywhere. So assert the agreement directly: one
// transaction, visible in *both* places.
test("a valid date is visible to the ledger and the balance alike", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank", "NZD", {
    opening_balance_minor: 1_000_00,
    opening_balance_date: "2026-01-01",
  });
  const tx = await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-07-31",
    amount_minor: -250_00,
    description: "Groceries",
  });
  expect(tx.posted_at).toBe("2026-07-31");

  // In the list, both unfiltered and inside a window that brackets it (the `date()` filter
  // is the other place a malformed date used to disappear).
  const all = await api.GET("/api/transactions", {});
  expect((all.data ?? []).map((t) => t.id)).toContain(tx.id);
  const windowed = await api.GET("/api/transactions", {
    params: { query: { from: "2026-07-01", to: "2026-07-31" } },
  });
  expect((windowed.data ?? []).map((t) => t.id)).toContain(tx.id);

  // And in the balance, which is the figure the row used to be missing from.
  const balances = await api.GET("/api/reports/balances", { params: { query: { to: "2026-08-31" } } });
  const row = balances.data!.accounts.find((a) => a.account_id === acc.id)!;
  expect(row.value_minor).toBe(750_00);
  expect(balances.data!.total_minor).toBe(750_00);
});

// ---- amounts the reports can actually add up --------------------------------------------
//
// The mirror image of the date bug above, and the same shape: nothing between the wire and the
// column constrained the *magnitude* of money, so `amount_minor: 9223372036854775807` was a
// **201**. Post two of them and the balance walk adds them together:
//
//   * in a debug build (`overflow-checks` on) that panics; `CatchPanicLayer` turns it into a
//     scrubbed 500, so the balance sheet, net worth, equity position and forecast all break at
//     once — and the rows responsible cannot be found through the UI, because the pages that
//     would list them are the ones 500ing;
//   * in a release build (the root `Cargo.toml` sets no `overflow-checks`) it wraps to a small
//     negative and the balance sheet prints a plausible, wrong number with no error anywhere.
//
// `sure_core::Money` now bounds every wire-edge money field at ±$1 trillion in minor units, and
// the report aggregation independently accumulates in i128 so a row written before that type
// existed saturates loudly instead of panicking or wrapping.

/** ±$1 trillion in minor units — `sure_core::MAX_MONEY_MINOR`, the largest legal amount. */
const MAX_MONEY_MINOR = 1_000_000_000_000_00;

test("a transaction amount the reports cannot add up is refused, not stored", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank", "NZD", {
    opening_balance_minor: 1_000_00,
    opening_balance_date: "2026-01-01",
  });

  // `9223372036854775807` is the literal from the report. It is past `Number.MAX_SAFE_INTEGER`,
  // so JS widens it — which is itself part of the point: no spelling of "absurdly large" is a
  // 201 any more, whether the bound or `i64`'s own range catches it first.
  for (const amount_minor of [
    9223372036854775807,
    -9223372036854775808,
    MAX_MONEY_MINOR + 1,
    -(MAX_MONEY_MINOR + 1),
    Number.MAX_SAFE_INTEGER,
  ]) {
    const { response, error } = await api.POST("/api/transactions", {
      body: { account_id: acc.id, posted_at: "2026-07-31", amount_minor, description: "absurd" },
    });
    // 422, not the 201 this used to be. As with the dates, the envelope's code is the
    // framework's rejection slug — the field-level message lives in `sure-core`'s own tests.
    expect(response.status, String(amount_minor)).toBe(422);
    expect(error?.error.code, String(amount_minor)).toBe("validation");
  }

  // Nothing was written, and — the assertion that actually matters — the reports that used to
  // 500 (or lie) still answer with the true balance.
  const list = await api.GET("/api/transactions", {});
  expect((list.data ?? []).some((t) => t.description === "absurd")).toBe(false);

  const balances = await api.GET("/api/reports/balances", { params: { query: { to: "2026-08-31" } } });
  expect(balances.response.status).toBe(200);
  expect(balances.data!.accounts.find((a) => a.account_id === acc.id)!.value_minor).toBe(1_000_00);

  const netWorth = await api.GET("/api/reports/net-worth", { params: { query: { to: "2026-08-31" } } });
  expect(netWorth.response.status).toBe(200);

  // The ceiling itself is a legal amount — the bound is four orders of magnitude above any real
  // household figure precisely so that it never argues with real data.
  const atCeiling = await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-07-31",
    amount_minor: MAX_MONEY_MINOR,
    description: "at the ceiling",
  });
  expect(atCeiling.amount_minor).toBe(MAX_MONEY_MINOR);
  const after = await api.GET("/api/reports/balances", { params: { query: { to: "2026-08-31" } } });
  expect(after.data!.accounts.find((a) => a.account_id === acc.id)!.value_minor).toBe(
    1_000_00 + MAX_MONEY_MINOR
  );
});

test("an absurd amount is refused on every write that carries one", async ({ api }) => {
  const house = await createAccount(api, "House", "real_estate");
  const checking = await createAccount(api, "Checking", "bank");
  const savings = await createAccount(api, "Savings", "savings");
  const over = MAX_MONEY_MINOR + 1;

  const valuation = await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: house.id } },
    body: { as_of: "2026-07-31", value_minor: over },
  });
  expect(valuation.response.status).toBe(422);
  // Names the field and the bound, so a caller can tell a scaled-twice figure from a typo
  // rather than being told only that something was unprocessable.
  expect(valuation.error?.error.message).toContain("value_minor");
  expect((await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: house.id } } })).data)
    .toHaveLength(0);

  // `i64::MIN` is the sharpest case of all: the transfer writer normalises direction with
  // `.abs()`, which panicked in debug and returned `i64::MIN` unchanged in release — and the
  // outflow leg then negated it again, so a release build stored two wrapped legs and answered
  // 201. It has to be a clean 422 instead.
  const transfer = await api.POST("/api/transfers", {
    body: {
      from_account_id: checking.id,
      to_account_id: savings.id,
      posted_at: "2026-03-01",
      from_amount_minor: -9223372036854775808,
      description: "Move to savings",
    },
  });
  expect(transfer.response.status).toBe(422);
  expect((await api.GET("/api/transactions", {})).data).toHaveLength(0);

  // The destination leg carries its own amount for a cross-currency transfer, so it is bounded
  // separately — a source within the ceiling must not be able to smuggle a destination past it.
  const lopsided = await api.POST("/api/transfers", {
    body: {
      from_account_id: checking.id,
      to_account_id: savings.id,
      posted_at: "2026-03-01",
      from_amount_minor: 25_000,
      to_amount_minor: over,
      description: "Move to savings",
    },
  });
  expect(lopsided.response.status).toBe(422);

  // A cron re-posts its amount *every period*, so an absurd one compounds unattended rather
  // than being one bad row someone might spot.
  const cron = await api.POST("/api/crons", {
    body: {
      name: "Salary",
      account_id: checking.id,
      kind: "fixed_transaction",
      amount_minor: over,
      start_date: "2026-01-01",
    },
  });
  expect(cron.response.status).toBe(422);

  // The amount is on the *effect* now, not the event, so this is the bound `effect_amounts_in_range`
  // enforces across every effect in the body. (Same stale-shape story as the date test above: the
  // old flat body made this a 422 for failing to deserialise at all.)
  const event = await api.POST("/api/forecast/events", {
    body: {
      label: "Roof",
      kind: "adjustment",
      expected_on: "2026-07-31",
      effects: [
        {
          kind: "one_off_amount",
          target: { kind: "account", account_id: house.id },
          amount_minor: -over,
        },
      ],
    },
  });
  expect(event.response.status).toBe(422);
});
