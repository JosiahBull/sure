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
  const acc = await createAccount(api, "Meridian", "shares_nz");
  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id } },
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
