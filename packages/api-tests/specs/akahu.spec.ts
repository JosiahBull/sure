import { test, expect } from "../fixtures";
import { createAccount } from "../helpers";

// These tests deliberately never set AKAHU_APP_TOKEN / AKAHU_USER_TOKEN (the fixture
// strips them even if the developer's shell has them exported — see fixtures.ts) — CI has
// no live Akahu credentials, so only the "not configured" paths and the pure
// discover-then-link persistence logic are exercised here. A real sync against the live
// API is a manual smoke test, not something this suite can assert on.

test("provider kinds list akahu as credential-based and discovery-capable", async ({ api }) => {
  const kinds = await api.GET("/api/provider-kinds", {});
  const akahu = kinds.data?.find((k) => k.kind === "akahu");
  expect(akahu?.accepts_payload).toBe(false);
  expect(akahu?.supports_account_discovery).toBe(true);
});

test("discovering akahu accounts without credentials fails clearly", async ({ api }) => {
  const { response, error } = await api.GET("/api/provider-kinds/{kind}/accounts", {
    params: { path: { kind: "akahu" } },
  });
  expect(response.status).toBe(422);
  expect((error as { error?: { message?: string } })?.error?.message).toContain("AKAHU_APP_TOKEN");
});

test("discovering an unknown provider kind fails clearly", async ({ api }) => {
  const { response } = await api.GET("/api/provider-kinds/{kind}/accounts", {
    params: { path: { kind: "nope" } },
  });
  expect(response.status).toBe(422);
});

test("syncing an akahu provider without credentials fails and is recorded", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const { data: provider, response: createRes } = await api.POST("/api/providers", {
    body: {
      name: "Akahu — Everyday",
      kind: "akahu",
      account_id: acc.id,
      enabled: true,
      config: { external_account_id: "acc_manual" },
    },
  });
  expect(createRes.status).toBe(201);

  const { response: syncRes } = await api.POST("/api/providers/{id}/sync", {
    params: { path: { id: provider!.id } },
    body: {},
  });
  expect(syncRes.status).toBe(422);

  const syncs = await api.GET("/api/providers/{id}/syncs", { params: { path: { id: provider!.id } } });
  expect(syncs.data?.length).toBe(1);
  expect(syncs.data![0].status).toBe("error");
});

test("linking a discovered account creates a new local account atomically", async ({ api }) => {
  const before = await api.GET("/api/accounts", {});
  const beforeCount = before.data?.length ?? 0;

  const { data: provider, response } = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_new_1",
      name: "Akahu — Spending",
      new_account: {
        name: "Spending",
        kind: "bank",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  expect(response.status).toBe(201);
  expect(provider?.kind).toBe("akahu");
  expect((provider?.config as { external_account_id?: string })?.external_account_id).toBe("acc_new_1");

  const after = await api.GET("/api/accounts", {});
  expect(after.data?.length).toBe(beforeCount + 1);
  const created = after.data?.find((a) => a.name === "Spending");
  expect(created).toBeTruthy();
  expect(created?.id).toBe(provider?.account_id);
});

test("linking triggers an immediate sync attempt rather than waiting for the next poll", async ({ api }) => {
  const { data: provider, response } = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_autosync_1",
      name: "Akahu — Autosync",
      new_account: {
        name: "Autosync",
        kind: "bank",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  // Linking itself still succeeds even though the initial sync fails (no live
  // credentials in CI) — the failed attempt must not undo the just-created link.
  expect(response.status).toBe(201);

  const syncs = await api.GET("/api/providers/{id}/syncs", { params: { path: { id: provider!.id } } });
  // A sync row already exists without ever calling POST /providers/{id}/sync — proving
  // linking triggered it automatically rather than requiring a manual "Sync now" first.
  expect(syncs.data?.length).toBe(1);
  expect(syncs.data![0].status).toBe("error");
});

test("linking to an existing account doesn't create a new one", async ({ api }) => {
  const acc = await createAccount(api, "Savings", "savings");
  const before = await api.GET("/api/accounts", {});
  const beforeCount = before.data?.length ?? 0;

  const { data: provider, response } = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_existing_1",
      name: "Akahu — Savings",
      existing_account_id: acc.id,
    },
  });
  expect(response.status).toBe(201);
  expect(provider?.account_id).toBe(acc.id);

  const after = await api.GET("/api/accounts", {});
  expect(after.data?.length).toBe(beforeCount);
});

test("linking requires exactly one of new_account or existing_account_id", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");

  const neither = await api.POST("/api/providers/link", {
    body: { kind: "akahu", external_id: "acc_x", name: "x" },
  });
  expect(neither.response.status).toBe(422);

  const both = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_y",
      name: "y",
      existing_account_id: acc.id,
      new_account: {
        name: "z",
        kind: "bank",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  expect(both.response.status).toBe(422);
});

test("linking with an unknown provider kind is rejected", async ({ api }) => {
  const { response } = await api.POST("/api/providers/link", {
    body: { kind: "nope", external_id: "acc_x", name: "x", existing_account_id: 1 },
  });
  expect(response.status).toBe(422);
});
