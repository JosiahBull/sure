import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

const securedBy = (api: SureClient, id: number, target: number | null) =>
  api.PUT("/api/accounts/{id}/secured-by", {
    params: { path: { id } },
    body: { secured_by_account_id: target },
  });
const del = (api: SureClient, id: number) =>
  api.DELETE("/api/accounts/{id}", { params: { path: { id } } });
const get = (api: SureClient, id: number) =>
  api.GET("/api/accounts/{id}", { params: { path: { id } } });

// An asset acts as a "parent" for the debts secured against it.

test("deleting an asset is blocked while a debt is secured against it", async ({ api }) => {
  const house = await createAccount(api, "Home", "real_estate");
  const mortgage = await createAccount(api, "Mortgage", "mortgage");
  await securedBy(api, mortgage.id, house.id);

  const blocked = await del(api, house.id);
  expect(blocked.response.status).toBe(409);
  // The error names the offending debt so the user knows what to unlink/delete.
  expect(blocked.error?.error.message).toContain("Mortgage");
  // And the asset is untouched.
  expect((await get(api, house.id)).response.status).toBe(200);

  // Unlinking the debt frees the asset for deletion.
  await securedBy(api, mortgage.id, null);
  expect((await del(api, house.id)).response.status).toBe(204);
});

test("deleting the secured debt first frees the asset", async ({ api }) => {
  const house = await createAccount(api, "Home", "real_estate");
  const mortgage = await createAccount(api, "Mortgage", "mortgage");
  await securedBy(api, mortgage.id, house.id);

  // The debt is the child — it deletes freely...
  expect((await del(api, mortgage.id)).response.status).toBe(204);
  // ...and now the asset does too.
  expect((await del(api, house.id)).response.status).toBe(204);
});

test("an account with no dependents deletes normally", async ({ api }) => {
  const acc = await createAccount(api, "Spare", "bank");
  expect((await del(api, acc.id)).response.status).toBe(204);
  expect((await get(api, acc.id)).response.status).toBe(404);
});
