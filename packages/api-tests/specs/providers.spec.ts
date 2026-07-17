import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

const CSV = `date,amount,description,external_id
2026-01-05,-12.50,Coffee,c1
2026-01-06,-40.00,Groceries,c2
2026-01-07,2500.00,Salary,c3
`;

const csvProvider = async (api: SureClient, accountId: number) => {
  const { data, response } = await api.POST("/api/providers", {
    body: { name: "Bank CSV", kind: "csv", account_id: accountId, enabled: true },
  });
  expect(response.status).toBe(201);
  return data!;
};

test("provider kinds list the CSV importer", async ({ api }) => {
  const kinds = await api.GET("/api/provider-kinds", {});
  const csv = kinds.data?.find((k) => k.kind === "csv");
  expect(csv?.accepts_payload).toBe(true);
});

test("an unknown provider kind is rejected", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const { response } = await api.POST("/api/providers", {
    body: { name: "x", kind: "nope", account_id: acc.id, enabled: true },
  });
  expect(response.status).toBe(422);
});

test("CSV sync imports then dedupes on re-sync", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const provider = await csvProvider(api, acc.id);

  const first = await api.POST("/api/providers/{id}/sync", { params: { path: { id: provider.id } }, body: { payload: CSV } });
  expect(first.data?.imported).toBe(3);
  expect(first.data?.skipped).toBe(0);
  expect(first.data?.status).toBe("ok");

  const txns = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
  expect(txns.data?.length).toBe(3);
  expect(txns.data?.find((t) => t.description === "Salary")?.amount_minor).toBe(250_000);
  expect(txns.data?.find((t) => t.description === "Coffee")?.amount_minor).toBe(-1250);

  const second = await api.POST("/api/providers/{id}/sync", { params: { path: { id: provider.id } }, body: { payload: CSV } });
  expect(second.data?.imported).toBe(0);
  expect(second.data?.skipped).toBe(3);

  const syncs = await api.GET("/api/providers/{id}/syncs", { params: { path: { id: provider.id } } });
  expect(syncs.data?.length).toBe(2);
});

test("CSV sync resolves a merchant column to a reusable Merchant", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const provider = await csvProvider(api, acc.id);
  const csv = `date,amount,description,merchant,external_id
2026-01-05,-4.50,Flat white,The Roastery,m1
2026-01-06,-5.00,Long black,The Roastery,m2
`;
  const res = await api.POST("/api/providers/{id}/sync", {
    params: { path: { id: provider.id } },
    body: { payload: csv },
  });
  expect(res.data?.imported).toBe(2);

  const merchants = await api.GET("/api/merchants", {});
  const matches = merchants.data?.filter((m) => m.name === "The Roastery") ?? [];
  expect(matches.length).toBe(1); // reused across both rows, not duplicated

  const txns = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
  const roasteryTxns = txns.data?.filter((t) => t.merchant === "The Roastery") ?? [];
  expect(roasteryTxns.length).toBe(2);
  expect(roasteryTxns.every((t) => t.merchant_id === matches[0]?.id)).toBe(true);
});

test("CSV sync without a payload errors and is recorded", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const provider = await csvProvider(api, acc.id);

  const res = await api.POST("/api/providers/{id}/sync", { params: { path: { id: provider.id } }, body: {} });
  expect(res.response.status).toBe(422);

  const syncs = await api.GET("/api/providers/{id}/syncs", { params: { path: { id: provider.id } } });
  expect(syncs.data?.length).toBe(1);
  expect(syncs.data![0].status).toBe("error");
});
