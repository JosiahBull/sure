import { expect } from "@playwright/test";
import type { Schemas, SureClient } from "../client/src/index";

/** Create an account, asserting success, and return it. Fills the required flags. */
export async function createAccount(
  api: SureClient,
  name: string,
  kind: Schemas["SaveAccount"]["kind"],
  currency = "NZD"
) {
  const { data, response } = await api.POST("/api/accounts", {
    body: { name, kind, currency_code: currency, archived: false, sort_order: 0 },
  });
  expect(response.status, "create account").toBe(201);
  return data!;
}

export async function createCategory(
  api: SureClient,
  name: string,
  kind = "expense",
  parentId: number | null = null
) {
  const { data, response } = await api.POST("/api/categories", {
    body: { name, kind, parent_id: parentId, sort_order: 0 },
  });
  expect(response.status, "create category").toBe(201);
  return data!;
}

export async function createMerchant(api: SureClient, name: string, categoryId: number | null = null) {
  const { data, response } = await api.POST("/api/merchants", {
    body: { name, category_id: categoryId },
  });
  expect(response.status, "create merchant").toBe(201);
  return data!;
}

export async function createTransaction(
  api: SureClient,
  input: {
    account_id: number;
    posted_at: string;
    amount_minor: number;
    description?: string;
    category_id?: number | null;
    merchant_id?: number | null;
    is_one_off?: boolean;
  }
) {
  const { data, response } = await api.POST("/api/transactions", {
    body: {
      account_id: input.account_id,
      posted_at: input.posted_at,
      amount_minor: input.amount_minor,
      description: input.description ?? "x",
      category_id: input.category_id ?? null,
      merchant_id: input.merchant_id ?? null,
      is_one_off: input.is_one_off ?? false,
    },
  });
  expect(response.status, "create transaction").toBe(201);
  return data!;
}

export async function getTransaction(api: SureClient, id: number) {
  const { data, response } = await api.GET("/api/transactions/{id}", {
    params: { path: { id } },
  });
  expect(response.status).toBe(200);
  return data!;
}
