import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

/**
 * Valuations are how an account whose history nobody can import still gets a balance — a
 * student loan the lender only reports the current figure for, a car, a house. These cover
 * the two things the UI depends on: narrowing a series that grows by a row a day forever,
 * and the fact that a valuation is a *level* that governs from its date until the next one.
 */

const setValue = (api: SureClient, id: number, as_of: string, value_minor: number, note?: string) =>
  api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id } },
    body: { as_of, value_minor, note },
  });

test("valuations list newest first, and can be narrowed by source and count", async ({ api }) => {
  const acc = await createAccount(api, "Sunny", "vehicle", "NZD", {
    metadata: { profile: "vehicle", make: "Nissan", model: "Caravan", year: 1990 },
    opening_balance_minor: 16_500_00,
    opening_balance_date: "2025-01-27",
  });
  await setValue(api, acc.id, "2026-01-01", 15_900_00);
  await setValue(api, acc.id, "2026-06-01", 15_400_00);

  const all = (await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: acc.id } } }))
    .data!;
  expect(all.map((v) => v.as_of)).toEqual(["2026-06-01", "2026-01-01", "2025-01-27"]);
  // Created through the API, so every one of them is `manual` — including the opening balance.
  expect(new Set(all.map((v) => v.source))).toEqual(new Set(["manual"]));

  const newest = (
    await api.GET("/api/accounts/{id}/valuations", {
      params: { path: { id: acc.id }, query: { limit: 2 } },
    })
  ).data!;
  expect(newest.map((v) => v.as_of)).toEqual(["2026-06-01", "2026-01-01"]);

  const manual = (
    await api.GET("/api/accounts/{id}/valuations", {
      params: { path: { id: acc.id }, query: { source: "manual" } },
    })
  ).data!;
  expect(manual.length).toBe(3);
  const cron = (
    await api.GET("/api/accounts/{id}/valuations", {
      params: { path: { id: acc.id }, query: { source: "cron" } },
    })
  ).data!;
  expect(cron.length).toBe(0);
});

/**
 * The edge parse. A filter that silently matched everything on a typo would report a series
 * nobody asked for — and is the one behaviour a client-side filter could never provide.
 */
test("an unrecognised source is refused, not silently ignored", async ({ api }) => {
  const acc = await createAccount(api, "Sunny", "vehicle", "NZD", {
    metadata: { profile: "vehicle", make: "Nissan", model: "Caravan", year: 1990 },
  });
  const { response } = await api.GET("/api/accounts/{id}/valuations", {
    params: { path: { id: acc.id }, query: { source: "nonsense" } },
  });
  expect(response.status).toBe(422);
});

/**
 * The semantic the whole feature rests on, and the reason back-dating one fixes a history:
 * a valuation governs from its date until the next one, and before the earliest valuation the
 * account is worth nothing rather than being absent.
 *
 * This is exactly Ansam's student loan — a balance the lender reports today and no history at
 * all, which read as $0 for every date before the first sync.
 */
test("a back-dated balance applies from its date, and not before", async ({ api }) => {
  const loan = await createAccount(api, "Student loan", "student_loan", "NZD", {
    metadata: { profile: "student_loan" },
  });
  // Today's figure only — the shape a provider-linked loan arrives in.
  await setValue(api, loan.id, "2026-08-07", -59_020_76);

  const netWorthOn = async (on: string) => {
    const { data } = await api.GET("/api/reports/net-worth", {
      params: { query: { from: on, to: on, interval: "day" } },
    });
    return data!.points[data!.points.length - 1].net_worth_minor;
  };

  // Before the only valuation, the debt simply isn't on the books.
  expect(await netWorthOn("2026-01-01")).toBe(0);
  expect(await netWorthOn("2026-08-07")).toBe(-59_020_76);

  // Back-date one, and the history stops lying.
  await setValue(api, loan.id, "2020-01-01", -62_000_00, "opening balance, from the IR letter");
  expect(await netWorthOn("2020-01-01")).toBe(-62_000_00);
  // …and it is held at that level until the later one takes over — a valuation is a level
  // that carries forward, not a point.
  expect(await netWorthOn("2026-01-01")).toBe(-62_000_00);
  expect(await netWorthOn("2026-08-07")).toBe(-59_020_76);
  // Still nothing before the earliest valuation.
  expect(await netWorthOn("2019-12-31")).toBe(0);
});

test("a valuation can be deleted, and an unknown one is a 404", async ({ api }) => {
  const acc = await createAccount(api, "Sunny", "vehicle", "NZD", {
    metadata: { profile: "vehicle", make: "Nissan", model: "Caravan", year: 1990 },
  });
  const created = (await setValue(api, acc.id, "2026-06-01", 15_400_00)).data!;

  const del = await api.DELETE("/api/valuations/{id}", { params: { path: { id: created.id } } });
  expect(del.response.status).toBe(204);
  const after = (
    await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: acc.id } } })
  ).data!;
  expect(after.find((v) => v.id === created.id)).toBeUndefined();

  const missing = await api.DELETE("/api/valuations/{id}", { params: { path: { id: 999_999 } } });
  expect(missing.response.status).toBe(404);
});
