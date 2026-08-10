/**
 * The MCP endpoint, driven as what it is: JSON-RPC over one POST.
 *
 * Deliberately not through an MCP client library. The thing worth testing here is the
 * *mount* — that `/mcp` exists only when `SURE_MCP` says so, that the tier gate is a
 * registration gate rather than a runtime check, and that a tool call reaches the real
 * services and comes back with figures a reader could act on. A client library would sit
 * between the test and every one of those, and would need its own version pinned in step
 * with `rmcp`. The protocol at this level is a JSON object with a `method` in it.
 *
 * The tool contract itself — names, arguments, annotations — is pinned by
 * `packages/mcp/tool-manifest.json` and its snapshot test, which is far cheaper than
 * asserting it over HTTP.
 */
import { test, expect, startServer, createSureClient } from "../fixtures";
import type { StartedServer } from "../fixtures";

/** Streamable HTTP wants both content types offered, even in JSON-response mode. */
const HEADERS = {
  "Content-Type": "application/json",
  Accept: "application/json, text/event-stream",
};

type RpcResult = { result?: Record<string, unknown>; error?: { code: number; message: string } };

let nextId = 1;

async function rpc(
  server: StartedServer,
  method: string,
  params: Record<string, unknown> = {},
): Promise<RpcResult> {
  const res = await fetch(`${server.baseURL}/mcp`, {
    method: "POST",
    headers: HEADERS,
    body: JSON.stringify({ jsonrpc: "2.0", id: nextId++, method, params }),
  });
  expect(res.ok, `${method} returned HTTP ${res.status}`).toBe(true);
  return (await res.json()) as RpcResult;
}

/** The text a tool call came back with. Fails the test if the call itself errored. */
async function callTool(
  server: StartedServer,
  name: string,
  args: Record<string, unknown> = {},
): Promise<string> {
  const body = await rpc(server, "tools/call", { name, arguments: args });
  expect(body.error, `${name} failed: ${JSON.stringify(body.error)}`).toBeUndefined();
  const content = (body.result?.content ?? []) as { type: string; text?: string }[];
  return content.map((c) => c.text ?? "").join("\n");
}

/**
 * A server at `ceiling`, with the app's own setting turned up to `mode`.
 *
 * Both halves are needed now: `SURE_MCP` only sets the maximum, and a fresh database stores
 * `off`, so a server started with `SURE_MCP=write` alone serves nothing at all.
 */
async function startWithMode(
  ceiling: "read" | "write",
  mode: "read" | "write" = ceiling,
): Promise<StartedServer> {
  const server = await startServer({ SURE_MCP: ceiling });
  const api = createSureClient(server.baseURL);
  const { response } = await api.PUT("/api/settings", {
    body: { base_currency_code: "NZD", mcp_mode: mode },
  });
  expect(response.status, `could not set agent access to ${mode}`).toBe(200);
  return server;
}

async function toolNames(server: StartedServer): Promise<string[]> {
  const body = await rpc(server, "tools/list");
  const tools = (body.result?.tools ?? []) as { name: string }[];
  return tools.map((t) => t.name).sort();
}

test("the endpoint is absent unless SURE_MCP asks for it", async ({ server }) => {
  // The shared fixture sets no `SURE_MCP`, which is exactly the production default.
  const res = await fetch(`${server.baseURL}/mcp`, {
    method: "POST",
    headers: HEADERS,
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }),
  });
  expect(res.status).toBe(404);
  // …while the rest of the app is unaffected by the flag being off.
  const health = await fetch(`${server.baseURL}/api/health`);
  expect(health.ok).toBe(true);
});

test("a server refuses to start when SURE_MCP is not one of the three values", async () => {
  // A typo in an access-control variable must stop the process, not be guessed at. This is
  // the one env var in `sure-server` that is fatal rather than warn-and-default.
  await expect(startServer({ SURE_MCP: "readonly" })).rejects.toThrow();
});

test("the stored setting decides what is served, and takes effect immediately", async () => {
  // SURE_MCP is the ceiling; the app's own setting picks within it. A fresh database stores
  // `off`, so a server started at the write ceiling still serves nothing until asked to.
  const server = await startServer({ SURE_MCP: "write" });
  const api = createSureClient(server.baseURL);
  try {
    const initial = await api.GET("/api/settings", {});
    expect(initial.data?.mcp_mode).toBe("off");
    expect(initial.data?.mcp_ceiling).toBe("write");
    expect(initial.data?.mcp_effective).toBe("off");
    expect(await toolNames(server)).toEqual([]);

    // Off is off, not read-only: even a read tool is absent, and calling one fails.
    const whileOff = await rpc(server, "tools/call", {
      name: "list_accounts",
      arguments: {},
    });
    expect(whileOff.error?.message).toContain("not found");

    // Turn it on — no restart, and the very next call sees it.
    await api.PUT("/api/settings", {
      body: { base_currency_code: "NZD", mcp_mode: "read" },
    });
    const readNames = await toolNames(server);
    expect(readNames).toContain("summarize_spending");
    expect(readNames).not.toContain("bulk_categorize");

    // Up to write, still no restart.
    await api.PUT("/api/settings", {
      body: { base_currency_code: "NZD", mcp_mode: "write" },
    });
    expect(await toolNames(server)).toContain("bulk_categorize");

    // …and back off again, which has to empty the surface rather than leave reads behind.
    await api.PUT("/api/settings", {
      body: { base_currency_code: "NZD", mcp_mode: "off" },
    });
    expect(await toolNames(server)).toEqual([]);
  } finally {
    await server.stop();
  }
});

test("the environment caps what the settings page may choose", async () => {
  const server = await startWithMode("read");
  const api = createSureClient(server.baseURL);
  try {
    expect((await api.GET("/api/settings", {})).data?.mcp_ceiling).toBe("read");

    // Read is within the ceiling.
    const allowed = await api.PUT("/api/settings", {
      body: { base_currency_code: "NZD", mcp_mode: "read" },
    });
    expect(allowed.response.status).toBe(200);
    expect(allowed.data?.mcp_effective).toBe("read");

    // Write is not — and is refused rather than quietly stored and clamped, so the page
    // never shows a mode the server is not actually serving.
    const refused = await api.PUT("/api/settings", {
      body: { base_currency_code: "NZD", mcp_mode: "write" },
    });
    expect(refused.response.status).toBe(422);
    expect((await api.GET("/api/settings", {})).data?.mcp_mode).toBe("read");
    expect(await toolNames(server)).not.toContain("bulk_categorize");
  } finally {
    await server.stop();
  }
});

test("changing only the base currency leaves agent access alone", async () => {
  // The web page did exactly this before agent access existed, and a naive UPDATE would
  // have reset the mode to its column default on every currency change.
  const server = await startServer({ SURE_MCP: "write" });
  const api = createSureClient(server.baseURL);
  try {
    await api.PUT("/api/settings", {
      body: { base_currency_code: "NZD", mcp_mode: "write" },
    });
    await api.PUT("/api/settings", { body: { base_currency_code: "USD" } });
    const after = await api.GET("/api/settings", {});
    expect(after.data?.base_currency_code).toBe("USD");
    expect(after.data?.mcp_mode).toBe("write");
  } finally {
    await server.stop();
  }
});

test("read mode serves the read tools and none of the writing ones", async () => {
  const server = await startWithMode("read");
  try {
    const init = await rpc(server, "initialize", {
      protocolVersion: "2026-07-28",
      capabilities: {},
      clientInfo: { name: "api-tests", version: "0" },
    });
    // Identifies as this app rather than as the SDK it is built on.
    expect((init.result?.serverInfo as { name: string }).name).toBe("sure");
    expect(init.result?.instructions).toContain("decimal strings");

    const names = await toolNames(server);
    expect(names).toContain("summarize_spending");
    expect(names).toContain("search_transactions");
    // The gate: not registered, so not merely refused — absent from the list entirely, and
    // costing the caller no context.
    for (const write of ["bulk_categorize", "create_transaction", "run_rule", "save_rule"]) {
      expect(names, `${write} must not exist in read mode`).not.toContain(write);
    }

    // And calling one anyway is a protocol error, not a write.
    const attempted = await rpc(server, "tools/call", {
      name: "bulk_categorize",
      arguments: { ids: [1], category_id: 1, dry_run: false, expect_count: 1 },
    });
    expect(attempted.error).toBeTruthy();
  } finally {
    await server.stop();
  }
});

test("write mode adds the writing tools", async () => {
  const server = await startWithMode("write");
  try {
    const names = await toolNames(server);
    for (const write of [
      "bulk_categorize",
      "create_transaction",
      "create_category",
      "create_merchant",
      "record_valuation",
      "run_rule",
      "save_rule",
      "undo_rule_run",
      "update_transaction",
    ]) {
      expect(names, `${write} must exist in write mode`).toContain(write);
    }
  } finally {
    await server.stop();
  }
});

test("a read tool reports real balances as decimal amounts", async () => {
  const server = await startWithMode("read");
  const api = createSureClient(server.baseURL);
  try {
    const account = await api.POST("/api/accounts", {
      body: {
        name: "Everyday",
        kind: "bank",
        institution: "ANZ",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        opening_balance_minor: 1_234_50,
        opening_balance_date: "2026-01-01",
        ownership: { kind: "joint" },
      },
    });
    expect(account.response.status).toBe(201);

    const text = await callTool(server, "list_accounts");
    expect(text).toContain("Everyday");
    // The property the whole conversion layer exists for: the caller sees 1234.50, not the
    // 123450 minor units it is stored as. A model handed the latter reports $123,450.
    expect(text).toContain("1234.50");
    expect(text).not.toContain("123450");
  } finally {
    await server.stop();
  }
});

test("summarize_spending totals rather than listing, and names the axis it grouped on", async () => {
  const server = await startWithMode("read");
  const api = createSureClient(server.baseURL);
  try {
    const account = await api.POST("/api/accounts", {
      body: {
        name: "Everyday",
        kind: "bank",
        institution: "ANZ",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        opening_balance_minor: 0,
        opening_balance_date: "2026-01-01",
        ownership: { kind: "joint" },
      },
    });
    const accountId = account.data!.id;
    const groceries = await api.POST("/api/categories", {
      body: { name: "Groceries", kind: "expense", sort_order: 0 },
    });
    const categoryId = groceries.data!.id;

    for (const [date, minor] of [
      ["2026-03-02", -50_00],
      ["2026-03-14", -70_00],
      ["2026-04-02", -30_00],
    ] as const) {
      const created = await api.POST("/api/transactions", {
        body: {
          account_id: accountId,
          posted_at: date,
          amount_minor: minor,
          description: "Countdown",
          is_one_off: false,
          category_id: categoryId,
        },
      });
      expect(created.response.status).toBe(201);
    }

    const byCategory = await callTool(server, "summarize_spending", {
      group_by: "category",
      from: "2026-03-01",
      to: "2026-04-30",
    });
    expect(byCategory).toContain("Groceries");
    // 50 + 70 + 30, totalled server-side and rendered as one decimal figure.
    expect(byCategory).toContain("150.00");
    expect(byCategory).toContain("grouped by category");

    const byMonth = await callTool(server, "summarize_spending", {
      group_by: "month",
      from: "2026-03-01",
      to: "2026-04-30",
    });
    // Chronological, and split at the month boundary rather than lumped together.
    expect(byMonth.indexOf("2026-03")).toBeLessThan(byMonth.indexOf("2026-04"));
    expect(byMonth).toContain("120.00");
    expect(byMonth).toContain("30.00");
  } finally {
    await server.stop();
  }
});

test("a bulk write is refused until the caller confirms the count it was shown", async () => {
  const server = await startWithMode("write");
  const api = createSureClient(server.baseURL);
  try {
    const account = await api.POST("/api/accounts", {
      body: {
        name: "Everyday",
        kind: "bank",
        institution: "ANZ",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        opening_balance_minor: 0,
        opening_balance_date: "2026-01-01",
        ownership: { kind: "joint" },
      },
    });
    const accountId = account.data!.id;
    const category = await api.POST("/api/categories", {
      body: { name: "Fuel", kind: "expense", sort_order: 0 },
    });
    const categoryId = category.data!.id;

    const ids: number[] = [];
    for (const date of ["2026-05-01", "2026-05-02", "2026-05-03"]) {
      const created = await api.POST("/api/transactions", {
        body: {
          account_id: accountId,
          posted_at: date,
          amount_minor: -60_00,
          description: "Z ENERGY",
          is_one_off: false,
        },
      });
      ids.push(created.data!.id);
    }

    // 1. Default is a dry run — it reports, and changes nothing.
    const dry = await callTool(server, "bulk_categorize", {
      search: "z energy",
      category_id: categoryId,
    });
    expect(dry).toContain("Dry run");
    expect(dry).toContain("3 transaction(s) would be changed");
    const untouched = await api.GET("/api/transactions/{id}", {
      params: { path: { id: ids[0] } },
    });
    expect(untouched.data?.category_id ?? null).toBeNull();

    // 2. dry_run=false alone is not enough — the count has to come back too.
    const noCount = await rpc(server, "tools/call", {
      name: "bulk_categorize",
      arguments: { search: "z energy", category_id: categoryId, dry_run: false },
    });
    expect(noCount.error?.message).toContain("expect_count");

    // 3. A count that disagrees with what the filter now matches is refused. This is the
    //    guard that matters: it catches a filter whose result moved between look and write.
    const wrongCount = await rpc(server, "tools/call", {
      name: "bulk_categorize",
      arguments: {
        search: "z energy",
        category_id: categoryId,
        dry_run: false,
        expect_count: 2,
      },
    });
    expect(wrongCount.error?.message).toContain("refusing to write");
    const stillUntouched = await api.GET("/api/transactions/{id}", {
      params: { path: { id: ids[0] } },
    });
    expect(stillUntouched.data?.category_id ?? null).toBeNull();

    // 4. Matching count: the write lands.
    const applied = await callTool(server, "bulk_categorize", {
      search: "z energy",
      category_id: categoryId,
      dry_run: false,
      expect_count: 3,
    });
    expect(applied).toContain("Updated 3 transaction(s)");
    for (const id of ids) {
      const row = await api.GET("/api/transactions/{id}", { params: { path: { id } } });
      expect(row.data?.category_id).toBe(categoryId);
    }
  } finally {
    await server.stop();
  }
});

test("every named range the server advertises is one it accepts", async () => {
  // The gap this closes: the specs above pass explicit from/to, so a `range` value that no
  // longer deserialises would go unnoticed — which is exactly what happened when serde's
  // snake_case turned Last90Days into `last90_days` while every prompt, the conventions
  // resource and the server instructions all said `last_90_days`.
  const server = await startWithMode("read");
  try {
    for (const range of ["last_month", "last_90_days", "ytd", "last_12_months", "all_time"]) {
      const body = await rpc(server, "tools/call", {
        name: "summarize_spending",
        arguments: { group_by: "category", range },
      });
      expect(body.error, `range=${range} was rejected`).toBeUndefined();
    }
    // And a plausible-looking one that is not offered still fails, so the list stays closed.
    const bad = await rpc(server, "tools/call", {
      name: "summarize_spending",
      arguments: { group_by: "category", range: "last_3_months" },
    });
    expect(bad.error ?? (bad.result as { isError?: boolean })?.isError).toBeTruthy();
  } finally {
    await server.stop();
  }
});

test("ids and a filter together are refused rather than silently preferring one", async () => {
  const server = await startWithMode("write");
  try {
    const body = await rpc(server, "tools/call", {
      name: "bulk_categorize",
      arguments: { ids: [1, 2], search: "anything", category_id: 1 },
    });
    expect(body.error?.message).toContain("not both");
  } finally {
    await server.stop();
  }
});

test("resources and prompts are served, and the conventions explain the numbers", async () => {
  const server = await startWithMode("read");
  try {
    const resources = await rpc(server, "resources/list");
    const uris = ((resources.result?.resources ?? []) as { uri: string }[]).map((r) => r.uri);
    expect(uris).toContain("sure://conventions");
    expect(uris).toContain("sure://accounts");

    const read = await rpc(server, "resources/read", { uri: "sure://conventions" });
    const contents = (read.result?.contents ?? []) as { text: string }[];
    expect(contents[0]?.text).toContain("Do not divide or multiply them");

    const prompts = await rpc(server, "prompts/list");
    const names = ((prompts.result?.prompts ?? []) as { name: string }[]).map((p) => p.name);
    expect(names).toContain("monthly_review");
    expect(names).toContain("tidy_uncategorised");

    const got = await rpc(server, "prompts/get", {
      name: "explain_account",
      arguments: { account_id: 7 },
    });
    const messages = (got.result?.messages ?? []) as { content: { text: string } }[];
    expect(messages[0]?.content.text).toContain("account_id=7");
  } finally {
    await server.stop();
  }
});

test("an unknown transaction is a caller-facing error, not a scrubbed internal one", async () => {
  const server = await startWithMode("read");
  try {
    const body = await rpc(server, "tools/call", {
      name: "get_transaction",
      arguments: { id: 999_999 },
    });
    // -32602 is INVALID_PARAMS: the caller can fix this by asking for a different id, and
    // the message says so rather than being replaced with a generic internal-error line.
    expect(body.error?.code).toBe(-32602);
    expect(body.error?.message).toContain("not found");
  } finally {
    await server.stop();
  }
});
