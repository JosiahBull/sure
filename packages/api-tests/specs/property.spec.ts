import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

const value = (api: SureClient, id: number, valueMinor: number) =>
  api.POST("/api/accounts/{id}/valuations", { params: { path: { id } }, body: { as_of: "2026-01-01", value_minor: valueMinor } });

const securedBy = (api: SureClient, id: number, target: number | null) =>
  api.PUT("/api/accounts/{id}/secured-by", { params: { path: { id } }, body: { secured_by_account_id: target } });

const position = (api: SureClient, id: number) =>
  api.GET("/api/accounts/{id}/equity-position", { params: { path: { id }, query: { to: "2026-07-01" } } });

test("a house shows total debt, equity and paid-off %", async ({ api }) => {
  const house = await createAccount(api, "Family Home", "real_estate");
  const mortgage = await createAccount(api, "Home Loan", "mortgage");
  const revolving = await createAccount(api, "Revolving", "revolving_credit");

  await value(api, house.id, 100_000_000); // $1,000,000
  await value(api, mortgage.id, -60_000_000); // owe $600,000
  await value(api, revolving.id, -10_000_000); // owe $100,000

  await securedBy(api, mortgage.id, house.id);
  await securedBy(api, revolving.id, house.id);

  const pos = (await position(api, house.id)).data!;
  expect(pos.value_minor).toBe(100_000_000);
  expect(pos.total_debt_minor).toBe(70_000_000);
  expect(pos.equity_minor).toBe(30_000_000);
  expect(pos.paid_off_pct).toBe(30); // (1,000,000 − 700,000) / 1,000,000
  expect(pos.liabilities.length).toBe(2);

  // Unlink the revolving credit -> less debt, higher paid-off %.
  await securedBy(api, revolving.id, null);
  const after = (await position(api, house.id)).data!;
  expect(after.total_debt_minor).toBe(60_000_000);
  expect(after.paid_off_pct).toBe(40);
  expect(after.liabilities.length).toBe(1);
});

test("secured-by rejects self and unknown targets", async ({ api }) => {
  const house = await createAccount(api, "Home", "real_estate");
  expect((await securedBy(api, house.id, house.id)).response.status).toBe(422);
  expect((await securedBy(api, house.id, 999_999)).response.status).toBe(422);
});

test("a fully-unencumbered asset is 100% paid off; no valuation is 0%", async ({ api }) => {
  const house = await createAccount(api, "House", "real_estate");
  const noVal = (await position(api, house.id)).data!;
  expect(noVal.paid_off_pct).toBe(0); // no value yet

  await value(api, house.id, 50_000_000);
  const owned = (await position(api, house.id)).data!;
  expect(owned.paid_off_pct).toBe(100);
  expect(owned.equity_minor).toBe(50_000_000);
});
