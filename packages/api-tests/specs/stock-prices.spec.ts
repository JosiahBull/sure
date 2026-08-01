import { test, expect } from "../fixtures";
import { createAccount } from "../helpers";

// The route's success path calls the real (keyless) Yahoo Finance endpoint on a cache
// miss, so it isn't exercised here — same reasoning as akahu.spec.ts: CI shouldn't
// depend on a live third-party API. These specs only cover the paths that 404 before
// ever reaching the provider, which is everything this route can get wrong on its own.

test("404s for an unknown account", async ({ api }) => {
  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: 999_999 } },
  });
  expect(response.status).toBe(404);
});

test("404s for a non-shares account kind", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(404);
});

test("404s for a shares account with no ticker set", async ({ api }) => {
  // A listed holding can't be *created* without a ticker any more, so the only way to hold
  // one is the provider-link path, which validates in `ValidationMode::Linked` — exactly the
  // state a discovered account is in before its first sync fills anything in.
  const linked = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_no_ticker",
      name: "Akahu — Meridian",
      new_account: {
        name: "Meridian",
        kind: "shares_nz",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  expect(linked.response.status).toBe(201);

  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: linked.data!.account_id } },
  });
  expect(response.status).toBe(404);
});

test("404s for a shares_private account (no market ticker)", async ({ api }) => {
  const acc = await createAccount(api, "Startco Options", "shares_private", "USD", {
    metadata: { profile: "shares", ticker: "N/A" },
  });
  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(404);
});
