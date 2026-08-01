import { test, expect } from "../fixtures";
import type { Schemas, SureClient } from "../../client/src/index";
import { createAccount, createTransaction } from "../helpers";

type Ownership = Schemas["Ownership"];

const addPerson = (api: SureClient, name: string) =>
  api.POST("/api/people", { body: { name, sort_order: 0 } });

/** Descriptions of the transactions whose *effective* attribution is `attributed_to`. */
async function attributedTo(api: SureClient, attributed_to: string) {
  const { data, response } = await api.GET("/api/transactions", {
    params: { query: { attributed_to } },
  });
  expect(response.status, `filter attributed_to=${attributed_to}`).toBe(200);
  return (data ?? []).map((t) => t.description).sort();
}

async function spend(
  api: SureClient,
  account_id: number,
  description: string,
  ownership?: Ownership
) {
  return createTransaction(api, {
    account_id,
    posted_at: "2026-02-01",
    amount_minor: -1000,
    description,
    ...(ownership ? { ownership } : {}),
  });
}

test("a transaction inherits its account's owner, and an override wins either way", async ({
  api,
}) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const sam = (await addPerson(api, "Sam")).data!;
  const alexs = await createAccount(api, "Alex's card", "credit_card", "NZD", {
    ownership: { kind: "person", person_id: alex.id },
  });
  const shared = await createAccount(api, "Joint", "bank", "NZD", {
    ownership: { kind: "joint" },
  });

  await spend(api, alexs.id, "alex inherited");
  await spend(api, shared.id, "joint inherited");
  // The two reasons an override exists.
  await spend(api, shared.id, "sam's own thing on the joint account", {
    kind: "person",
    person_id: sam.id,
  });
  await spend(api, alexs.id, "groceries for both, on alex's card", { kind: "joint" });

  expect(await attributedTo(api, String(alex.id))).toEqual(["alex inherited"]);
  expect(await attributedTo(api, String(sam.id))).toEqual([
    "sam's own thing on the joint account",
  ]);
  expect(await attributedTo(api, "joint")).toEqual([
    "groceries for both, on alex's card",
    "joint inherited",
  ]);
});

test("re-attributing an account moves its whole un-overridden history", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const sam = (await addPerson(api, "Sam")).data!;
  const account = await createAccount(api, "Everyday", "bank", "NZD", {
    ownership: { kind: "person", person_id: alex.id },
  });
  await spend(api, account.id, "inherited");
  await spend(api, account.id, "pinned to sam", { kind: "person", person_id: sam.id });

  await api.PUT("/api/accounts/{id}/ownership", {
    params: { path: { id: account.id } },
    body: { ownership: { kind: "person", person_id: sam.id } },
  });

  expect(await attributedTo(api, String(alex.id))).toEqual([]);
  expect(await attributedTo(api, String(sam.id))).toEqual(["inherited", "pinned to sam"]);
});

test("an unparseable attribution filter is a 400, not everyone's spending", async ({ api }) => {
  const { response } = await api.GET("/api/transactions", {
    params: { query: { attributed_to: "everyone" } },
  });
  expect(response.status).toBe(400);
});

test("bulk update sets an override, and a null clears it", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const account = await createAccount(api, "Joint", "bank", "NZD", {
    ownership: { kind: "joint" },
  });
  const one = await spend(api, account.id, "one");
  const two = await spend(api, account.id, "two");
  const ids = [one.id, two.id];

  const set = await api.POST("/api/transactions/bulk-update", {
    body: { ids, ownership: { kind: "person", person_id: alex.id } },
  });
  expect(set.response.status).toBe(200);
  expect(set.data?.affected).toBe(2);
  expect(await attributedTo(api, String(alex.id))).toEqual(["one", "two"]);

  // A present `null` is "go back to following the account" — distinct from omitting it.
  const cleared = await api.POST("/api/transactions/bulk-update", {
    body: { ids, ownership: null },
  });
  expect(cleared.data?.affected).toBe(2);
  expect(await attributedTo(api, "joint")).toEqual(["one", "two"]);
});

test("an omitted ownership in a bulk update leaves the override alone", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const account = await createAccount(api, "Joint", "bank", "NZD", {
    ownership: { kind: "joint" },
  });
  const tx = await spend(api, account.id, "mine", { kind: "person", person_id: alex.id });

  // A bulk edit of something else entirely must not disturb the attribution.
  const res = await api.POST("/api/transactions/bulk-update", {
    body: { ids: [tx.id], is_one_off: true },
  });
  expect(res.response.status).toBe(200);
  expect(await attributedTo(api, String(alex.id))).toEqual(["mine"]);
});

test("an override naming nobody is refused", async ({ api }) => {
  const account = await createAccount(api, "Joint", "bank", "NZD", {
    ownership: { kind: "joint" },
  });
  const res = await api.POST("/api/transactions", {
    body: {
      account_id: account.id,
      posted_at: "2026-02-01",
      amount_minor: -1000,
      description: "x",
      ownership: { kind: "person", person_id: 4040 },
    },
  });
  expect(res.response.status).toBe(422);
});

test("an imported transaction has no override, so it follows its account", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const account = await createAccount(api, "Everyday", "bank", "NZD", {
    ownership: { kind: "person", person_id: alex.id },
  });
  const tx = await spend(api, account.id, "from a feed");
  // What a provider import writes: nothing at all about attribution.
  expect(tx.ownership ?? null).toBeNull();
  expect(await attributedTo(api, String(alex.id))).toEqual(["from a feed"]);
});

test("a config snapshot round-trips an attribution override", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const account = await createAccount(api, "Joint", "bank", "NZD", {
    ownership: { kind: "joint" },
  });
  await spend(api, account.id, "mine", { kind: "person", person_id: alex.id });
  await spend(api, account.id, "ours");

  const snapshot = await api.GET("/api/config/export", {});
  const imported = await api.POST("/api/config/import", { body: snapshot.data as never });
  expect(imported.response.status).toBe(200);

  expect(await attributedTo(api, String(alex.id))).toEqual(["mine"]);
  expect(await attributedTo(api, "joint")).toEqual(["ours"]);
});
