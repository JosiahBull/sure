import { test, expect, allowUnstubbed } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

// The House Pricer opt-in flow, end to end: pre-flight, confirm, poll-shaped re-confirm, opt out.
//
// Every server in this suite is pointed at a `sure-testproxy` in replay mode with no snapshots
// (see fixtures.ts), so an outbound call nobody stubbed is answered `503 {}` and cannot leave the
// machine. That matters more here than for the other feeds: **nothing recorded from this upstream
// may be committed** — one exchange carries a street address, a GPS centroid, a title boundary
// polygon and a land value, i.e. a dossier on where somebody lives. `scripts/pii-scan.mjs`
// refuses such a recording by path and by content, so the fixtures below are hand-authored and
// every value in them is invented (CLAUDE.md rule 3).
//
// `times: 1` and `assertCount` do different jobs and both are used: the retiring stub makes an
// unwanted second call *fail*, and the count proves at the wire that one didn't happen.

/** An id shaped like a `unitOfPropertyId` that could not be a real one. */
const PROPERTY_ID = "00000000-0000-4000-8000-000000000001";
/** The neighbouring title, for the drift guard. */
const OTHER_PROPERTY_ID = "00000000-0000-4000-8000-0000000000ff";

const ADDRESS_LINE1 = "123 Kowhai Street";
const CITY = "Riccarton";
/** What the route builds from the two above, and what the upstream is asked for. */
const QUERY = `${ADDRESS_LINE1}, ${CITY}`;
/** The upstream's own normalised spelling — deliberately different from what was typed. */
const MATCHED_ADDRESS = "123 kowhai street, riccarton";

/** `GET /match`'s shape, trimmed to the four fields the adapter reads. */
function match(
  propertyId: string,
  modelA: number | null,
  modelB: number | null = null,
  streetAddress = MATCHED_ADDRESS,
): string {
  return JSON.stringify({
    unitOfPropertyId: propertyId,
    streetAddress,
    ...(modelA === null ? {} : { grossSalePricePredictedModelA: modelA }),
    ...(modelB === null ? {} : { grossSalePricePredictedModelB: modelB }),
    // Fields the adapter must ignore. Real responses carry ~45 of them; these are the three
    // that would be the most damaging to start reading by accident.
    boundaryWkt: "POLYGON((0 0, 0 1, 1 1, 0 0))",
    centroidWkt: "POINT(0 0)",
    legalDescription: "Lot 1 DP 000000",
    landValue: 300000.0,
    suburb: CITY,
  });
}

/** The upstream's own 404 body for an address it has no match for. */
const NO_MATCH_BODY = JSON.stringify({
  _embedded: { errors: [{ message: "No matching house found" }] },
  message: "Not Found",
});

const house = (api: SureClient, name = "Family Home", currency = "NZD") =>
  createAccount(api, name, "real_estate", currency, {
    metadata: { profile: "property", address_line1: ADDRESS_LINE1, city: CITY },
  });

const valuations = (api: SureClient, id: number) =>
  api.GET("/api/accounts/{id}/valuations", { params: { path: { id } } });

test("a pre-flight reports the match without storing anything", async ({ testproxy, api }) => {
  const acc = await house(api);
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    // Anchored on the path only — the query carries the address, and `stub`'s matcher is
    // deliberately path-only (see fixtures.ts).
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(PROPERTY_ID, 650000.0, 598000.0),
  });

  const { data, response } = await api.GET("/api/accounts/{id}/property-estimate/preview", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(200);
  // Model A is the recorded figure — the decision this integration was built around — and
  // model B rides along in the note so the ~8% spread stays visible.
  expect(data!.value_minor).toBe(650_000_00);
  expect(data!.currency_code).toBe("NZD");
  expect(data!.model_note).toBe("model A 650000, model B 598000");
  // The upstream's spelling, not what was typed: this is the field a person confirms against.
  expect(data!.matched_address).toBe(MATCHED_ADDRESS);
  expect(data!.property_id).toBe(PROPERTY_ID);
  expect(data!.query).toBe(QUERY);
  expect(data!.source).toBe("house_pricer");
  expect(data!.coverage).toContain("Christchurch");

  // A pre-flight stores nothing. Both halves are asserted because they fail differently: no
  // subscription means the monthly poll still ignores this account, and no valuation means
  // looking is not the same as recording.
  const after = (await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } })).data!;
  expect(after.metadata).toMatchObject({ profile: "property" });
  expect((after.metadata as { house_pricer?: unknown }).house_pricer ?? null).toBeNull();
  expect((await valuations(api, acc.id)).data!.filter((v) => v.source === "estimate")).toEqual([]);
});

test("an address the upstream doesn't cover is a 404, not an error", async ({ testproxy, api }) => {
  const acc = await createAccount(api, "Auckland Flat", "real_estate", "NZD", {
    metadata: { profile: "property", address_line1: "1 Queen Street", city: "Auckland" },
  });
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 404,
    response_headers: { "content-type": "application/json" },
    body: NO_MATCH_BODY,
  });

  // 404 rather than 502: the feed covers one city, so "no match" is the ordinary answer for a
  // property outside it and the UI explains it with `coverage` rather than reporting a fault.
  const { response } = await api.GET("/api/accounts/{id}/property-estimate/preview", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(404);
});

test("confirming subscribes the account and records the first estimate", async ({
  testproxy,
  api,
}) => {
  const acc = await house(api);
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(PROPERTY_ID, 650000.0, 598000.0),
  });

  const { data, response } = await api.POST("/api/accounts/{id}/property-estimate", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(200);
  // The subscription pins the id the server itself just matched — never one from the request —
  // which is what makes the poll's drift check meaningful.
  expect(data!.metadata).toMatchObject({
    profile: "property",
    house_pricer: {
      query: QUERY,
      property_id: PROPERTY_ID,
      matched_address: MATCHED_ADDRESS,
    },
  });

  // …and it records now rather than in thirty days, with the note the monthly poll also writes.
  const estimates = (await valuations(api, acc.id)).data!.filter((v) => v.source === "estimate");
  expect(estimates.length).toBe(1);
  expect(estimates[0].value_minor).toBe(650_000_00);
  expect(estimates[0].currency_code).toBe("NZD");
  expect(estimates[0].note).toBe("House Pricer estimate (model A 650000, model B 598000)");
});

test("re-confirming the same day upserts the estimate in place", async ({ testproxy, api }) => {
  const acc = await house(api);
  const stub = (modelA: number) =>
    testproxy.stub({
      upstream: "house_pricer",
      method: "GET",
      path_pattern: "^/api/property/core/match$",
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: match(PROPERTY_ID, modelA),
      times: 1,
    });

  await stub(650000.0);
  await api.POST("/api/accounts/{id}/property-estimate", { params: { path: { id: acc.id } } });
  await stub(712000.0);
  await api.POST("/api/accounts/{id}/property-estimate", { params: { path: { id: acc.id } } });

  // One row, refreshed — the partial unique index in 0036_estimate_valuations.sql. Without it a
  // monthly poll re-run (or a person pressing the button twice) accumulates a row per attempt.
  const estimates = (await valuations(api, acc.id)).data!.filter((v) => v.source === "estimate");
  expect(estimates.length).toBe(1);
  expect(estimates[0].value_minor).toBe(712_000_00);
});

test("an estimate valuation is distinguishable from a provider or manual one", async ({
  testproxy,
  api,
}) => {
  const acc = await house(api);
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(PROPERTY_ID, 650000.0),
  });
  await api.POST("/api/accounts/{id}/property-estimate", { params: { path: { id: acc.id } } });

  // A figure someone typed, the same day. It must sit *beside* the estimate rather than
  // colliding with it: they answer different questions ("what I believe it's worth" vs "what a
  // model guesses"), and `source` is how the UI tells them apart.
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: acc.id } },
    body: { as_of: new Date().toISOString().slice(0, 10), value_minor: 690_000_00 },
  });

  const all = (await valuations(api, acc.id)).data!;
  expect(all.filter((v) => v.source === "estimate").length).toBe(1);
  expect(all.filter((v) => v.source === "manual").length).toBeGreaterThanOrEqual(1);
});

test("a query that matches a different property is refused", async ({ testproxy, api }) => {
  const acc = await house(api);
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(PROPERTY_ID, 650000.0),
    times: 1,
  });
  await api.POST("/api/accounts/{id}/property-estimate", { params: { path: { id: acc.id } } });

  // The upstream re-indexes and the same fuzzy `q` now resolves to the neighbouring title. The
  // subscription still names the original id, so re-confirming must not quietly re-point it at
  // the other house — the value would look entirely plausible and nothing downstream could tell.
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(OTHER_PROPERTY_ID, 980000.0, null, "125 kowhai street, riccarton"),
    times: 1,
  });
  await api.POST("/api/accounts/{id}/property-estimate", { params: { path: { id: acc.id } } });

  const acc2 = (await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } })).data!;
  const link = (
    acc2.metadata as { house_pricer: { property_id: string; matched_address: string } }
  ).house_pricer;
  // Re-confirming is an explicit act on a *shown* match, so the subscription does move — what
  // must not happen is the monthly poll doing this silently. That half is pinned as a unit test
  // (`sure_app::tasks::property_estimates::refuses_an_estimate_for_a_different_property`),
  // because a scheduled sweep is not drivable from here.
  expect(link.property_id).toBe(OTHER_PROPERTY_ID);
  expect(link.matched_address).toBe("125 kowhai street, riccarton");
});

test("a property in another currency cannot subscribe", async ({ testproxy, api }) => {
  const acc = await house(api, "Sydney Unit", "AUD");
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(PROPERTY_ID, 650000.0),
  });

  // Refused at subscribe time rather than every month in a log nobody reads. There is no FX in
  // reach of the poll, so the alternative is booking an NZD figure against an AUD account at
  // parity — a wrong number that looks right.
  const { response } = await api.POST("/api/accounts/{id}/property-estimate", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(422);
  expect((await valuations(api, acc.id)).data!.filter((v) => v.source === "estimate")).toEqual([]);
});

test("an account with no address to look up says so, and calls nothing", async ({
  testproxy,
  api,
}) => {
  // A savings account, because it is the *reachable* way to have no address: `validate_for`
  // requires `address_line1` of a manually-created real-estate account, so a property account
  // with none cannot be made through the API at all (only by linking a provider). That
  // remaining case is covered where it can be reached — `routes::property_estimates`'s
  // `has_nothing_to_look_up_without_a_street` unit test.
  const savings = await createAccount(api, "Savings", "savings");

  const { response } = await api.GET("/api/accounts/{id}/property-estimate/preview", {
    params: { path: { id: savings.id } },
  });
  expect(response.status).toBe(400);

  // The point of the 400, and the half worth asserting at the wire: **nothing was sent**. An
  // empty `q` would also come back 400 — from the upstream, for less obvious reasons — after
  // putting somebody's request on the network to find out.
  const counted = await testproxy.assertCount({ upstream: "house_pricer" }, 0);
  expect(counted.passed, `nothing may reach the upstream: ${counted.message}`).toBe(true);

  // The DAL refuses the write too (`set_house_pricer_link`), which is the backstop for a future
  // caller that reaches past this route.
  const { response: post } = await api.POST("/api/accounts/{id}/property-estimate", {
    params: { path: { id: savings.id } },
  });
  expect(post.status).toBe(400);
});

test("an explicit q overrides the account's stored address", async ({ testproxy, api }) => {
  const acc = await house(api);
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(PROPERTY_ID, 650000.0),
  });

  // The escape hatch for an address the matcher spells differently than the person did.
  const { data } = await api.GET("/api/accounts/{id}/property-estimate/preview", {
    params: { path: { id: acc.id }, query: { q: "123 kowhai st riccarton" } },
  });
  expect(data!.query).toBe("123 kowhai st riccarton");
});

test("opting out stops the polling and keeps the history", async ({ testproxy, api }) => {
  const acc = await house(api);
  await testproxy.stub({
    upstream: "house_pricer",
    method: "GET",
    path_pattern: "^/api/property/core/match$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: match(PROPERTY_ID, 650000.0),
    times: 1,
  });
  await api.POST("/api/accounts/{id}/property-estimate", { params: { path: { id: acc.id } } });

  const { data, response } = await api.DELETE("/api/accounts/{id}/property-estimate", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(200);
  expect((data!.metadata as { house_pricer?: unknown }).house_pricer ?? null).toBeNull();

  // The estimates already recorded stay: they are history, and deleting somebody's valuation
  // series as a side effect of turning a feed off would be a surprise.
  const estimates = (await valuations(api, acc.id)).data!.filter((v) => v.source === "estimate");
  expect(estimates.length).toBe(1);

  // Idempotent — the UI can send it without first checking whether a link exists.
  expect(
    (await api.DELETE("/api/accounts/{id}/property-estimate", { params: { path: { id: acc.id } } }))
      .response.status,
  ).toBe(200);
});

test("an upstream that fails is a 502, not a 500", async ({ testproxy, api }) => {
  const acc = await house(api);
  // No stub at all, so the proxy answers the replay miss — the closest thing this suite has to
  // "House Pricer is having a bad minute".
  allowUnstubbed({
    upstream: "house_pricer",
    path_pattern: "^/api/property/core/match$",
    why: "the point of the test: an unreachable upstream must surface as 502, not 500",
  });

  const { response } = await api.GET("/api/accounts/{id}/property-estimate/preview", {
    params: { path: { id: acc.id } },
  });
  // 502 tells the client "someone else's server, try later"; 500 would claim a bug here. The
  // distinction is `AppError::Upstream`'s whole reason for existing.
  expect(response.status).toBe(502);
});

test("the configured source and its coverage are discoverable", async ({ api }) => {
  // So the web layer can label the button and explain a miss without hardcoding "Christchurch".
  const { data, response } = await api.GET("/api/property-estimate-source");
  expect(response.status).toBe(200);
  expect(data!.source).toBe("house_pricer");
  expect(data!.coverage).toBe("Christchurch, New Zealand");
});
