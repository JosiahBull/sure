import { test, expect } from "../fixtures";
import type { Schemas, SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

type Ownership = Schemas["Ownership"];

const addPerson = (api: SureClient, name: string, color?: string) =>
  api.POST("/api/people", { body: { name, color, sort_order: 0 } });
const delPerson = (api: SureClient, id: number) =>
  api.DELETE("/api/people/{id}", { params: { path: { id } } });
const listPeople = (api: SureClient) => api.GET("/api/people", {});
const getAccount = (api: SureClient, id: number) =>
  api.GET("/api/accounts/{id}", { params: { path: { id } } });
const attribute = (api: SureClient, id: number, ownership: Ownership) =>
  api.PUT("/api/accounts/{id}/ownership", { params: { path: { id } }, body: { ownership } });

/** The stand-in the household-required migration leaves in every database. */
async function placeholder(api: SureClient) {
  const people = (await listPeople(api)).data ?? [];
  const found = people.find((p) => p.placeholder);
  expect(found, "every database starts with a placeholder person").toBeDefined();
  return found!;
}

test("a fresh database has a placeholder person, so an account always has someone to belong to", async ({
  api,
}) => {
  const people = (await listPeople(api)).data ?? [];
  expect(people.length).toBe(1);
  expect(people[0].placeholder).toBe(true);
  expect(people[0].name).toBe("Unassigned");
});

test("people are CRUD-able and named uniquely", async ({ api }) => {
  const created = await addPerson(api, "Alex", "#7c5cff");
  expect(created.response.status).toBe(201);
  expect(created.data?.name).toBe("Alex");
  expect(created.data?.color).toBe("#7c5cff");
  // Someone the user added is never a placeholder.
  expect(created.data?.placeholder).toBe(false);

  // Case-insensitively unique: two people called "Alex" is a typo, not a household.
  const dupe = await addPerson(api, "alex");
  expect(dupe.response.status).toBe(409);

  const renamed = await api.PUT("/api/people/{id}", {
    params: { path: { id: created.data!.id } },
    body: { name: "Alexandra", sort_order: 1 },
  });
  expect(renamed.response.status).toBe(200);
  expect(renamed.data?.name).toBe("Alexandra");
  // Colour is part of the replace body — omitting it clears it, like every other PUT here.
  expect(renamed.data?.color ?? null).toBeNull();

  expect((await delPerson(api, created.data!.id)).response.status).toBe(204);
});

test("a colour has to be a colour", async ({ api }) => {
  const res = await addPerson(api, "Alex", "red; background: url(evil)");
  expect(res.response.status).toBe(422);
});

/** The whole point of the hard requirement: no account can exist without an owner. */
test("an account cannot be created without saying who it belongs to", async ({ api }) => {
  const res = await api.POST("/api/accounts", {
    body: {
      name: "Ownerless",
      kind: "bank",
      currency_code: "NZD",
      institution: "ANZ",
      archived: false,
      sort_order: 0,
      opening_balance_minor: 0,
      opening_balance_date: "2020-01-01",
      // no `ownership`
    } as never,
  });
  expect(res.response.status).toBe(422);
});

test("the same holds on update — a full replace has to name an owner too", async ({ api }) => {
  const account = await createAccount(api, "Everyday", "bank");
  const res = await api.PUT("/api/accounts/{id}", {
    params: { path: { id: account.id } },
    body: {
      name: "Everyday (renamed)",
      kind: "bank",
      currency_code: "NZD",
      institution: "ANZ",
      archived: false,
      sort_order: 0,
    } as never,
  });
  expect(res.response.status).toBe(422);
  // ...and the account is untouched.
  expect((await getAccount(api, account.id)).data?.name).toBe("Everyday");
});

test("an account is attributed at creation, and can be moved afterwards", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const sam = (await addPerson(api, "Sam")).data!;

  const created = await api.POST("/api/accounts", {
    body: {
      name: "Alex's everyday",
      kind: "bank",
      currency_code: "NZD",
      institution: "ANZ",
      archived: false,
      sort_order: 0,
      opening_balance_minor: 0,
      opening_balance_date: "2020-01-01",
      ownership: { kind: "person", person_id: alex.id },
    },
  });
  expect(created.response.status).toBe(201);
  expect(created.data?.ownership).toEqual({ kind: "person", person_id: alex.id });

  const moved = await attribute(api, created.data!.id, { kind: "person", person_id: sam.id });
  expect(moved.response.status).toBe(200);
  expect(moved.data?.ownership).toEqual({ kind: "person", person_id: sam.id });

  const shared = await attribute(api, created.data!.id, { kind: "joint" });
  expect(shared.data?.ownership).toEqual({ kind: "joint" });
});

test("attributing to someone who doesn't exist is refused", async ({ api }) => {
  const account = await createAccount(api, "Everyday", "bank");
  const res = await attribute(api, account.id, { kind: "person", person_id: 4040 });
  expect(res.response.status).toBe(422);
});

test("bulk attribution moves a whole selection, or none of it", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const one = await createAccount(api, "Everyday", "bank");
  const two = await createAccount(api, "Savings", "savings");

  const ok = await api.POST("/api/accounts/ownership", {
    body: { account_ids: [one.id, two.id], ownership: { kind: "person", person_id: alex.id } },
  });
  expect(ok.response.status).toBe(200);
  expect(ok.data?.affected).toBe(2);
  expect((await getAccount(api, two.id)).data?.ownership).toEqual({
    kind: "person",
    person_id: alex.id,
  });

  // One bad id fails the batch, and the good ids keep the owner they had.
  const partial = await api.POST("/api/accounts/ownership", {
    body: { account_ids: [one.id, 9999], ownership: { kind: "joint" } },
  });
  expect(partial.response.status).toBe(404);
  expect((await getAccount(api, one.id)).data?.ownership).toEqual({
    kind: "person",
    person_id: alex.id,
  });
});

test("someone who still owns an account can't be deleted", async ({ api }) => {
  const alex = (await addPerson(api, "Alex")).data!;
  const account = await createAccount(api, "Everyday", "bank");
  await attribute(api, account.id, { kind: "person", person_id: alex.id });

  const blocked = await delPerson(api, alex.id);
  expect(blocked.response.status).toBe(409);
  // The message names what's in the way, like the secured-debt refusal does.
  expect(blocked.error?.error.message).toContain("Everyday");

  await attribute(api, account.id, { kind: "joint" });
  expect((await delPerson(api, alex.id)).response.status).toBe(204);
});

/** Emptying the household would leave a database in which no account can be created. */
test("the last person can't be removed", async ({ api }) => {
  const only = await placeholder(api);
  const blocked = await delPerson(api, only.id);
  expect(blocked.response.status).toBe(409);
  expect(blocked.error?.error.message).toContain("at least one person");

  // With someone else in the household it goes through.
  await addPerson(api, "Alex");
  expect((await delPerson(api, only.id)).response.status).toBe(204);
});

/** Renaming the stand-in is the other way of answering "whose are these?" — "mine". */
test("renaming the placeholder clears its placeholder flag", async ({ api }) => {
  const stand_in = await placeholder(api);
  const renamed = await api.PUT("/api/people/{id}", {
    params: { path: { id: stand_in.id } },
    body: { name: "Josiah", color: "#7c5cff", sort_order: 0 },
  });
  expect(renamed.response.status).toBe(200);
  expect(renamed.data?.placeholder).toBe(false);
  expect(((await listPeople(api)).data ?? []).some((p) => p.placeholder)).toBe(false);
});

test("a config snapshot round-trips the household and its attributions", async ({ api }) => {
  const alex = (await addPerson(api, "Alex", "#7c5cff")).data!;
  const account = await createAccount(api, "Everyday", "bank", "NZD", {
    ownership: { kind: "person", person_id: alex.id },
  });

  const snapshot = await api.GET("/api/config/export", {});
  expect(snapshot.response.status).toBe(200);
  // The placeholder plus Alex.
  expect((snapshot.data as { people: unknown[] }).people.length).toBe(2);

  // Mutate, then restore.
  await addPerson(api, "Sam");
  const imported = await api.POST("/api/config/import", { body: snapshot.data as never });
  expect(imported.response.status).toBe(200);
  expect((imported.data as { counts: { people: number } }).counts.people).toBe(2);

  const people = (await listPeople(api)).data ?? [];
  expect(people.length).toBe(2);
  expect(people.some((p) => p.name === "Sam")).toBe(false);
  expect(people.find((p) => p.id === alex.id)?.color).toBe("#7c5cff");
  expect((await getAccount(api, account.id)).data?.ownership).toEqual({
    kind: "person",
    person_id: alex.id,
  });
});

/**
 * A backup taken before accounts had owners is still perfectly good data. Importing it must
 * not fail, and must not invent an owner either — it gets the same stand-in the migration
 * uses, so the invariant holds and the question stays visible.
 */
test("a pre-household snapshot imports onto a fresh placeholder", async ({ api }) => {
  const account = await createAccount(api, "Everyday", "bank");
  const snapshot = (await api.GET("/api/config/export", {})).data as Record<string, unknown>;

  const legacy = { ...snapshot };
  delete legacy.people;
  legacy.accounts = (legacy.accounts as Record<string, unknown>[]).map((a) => {
    const { ownership, person_id, ...rest } = a;
    return rest;
  });

  const imported = await api.POST("/api/config/import", { body: legacy as never });
  expect(imported.response.status).toBe(200);

  const restored = (await getAccount(api, account.id)).data!;
  expect(restored.ownership.kind).toBe("person");
  const owner = (await listPeople(api)).data!.find(
    (p) => restored.ownership.kind === "person" && p.id === restored.ownership.person_id
  );
  expect(owner?.placeholder).toBe(true);
});
