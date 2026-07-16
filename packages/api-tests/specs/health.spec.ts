import { test, expect } from "../fixtures";

test("health reports ok", async ({ api }) => {
  const { data, response } = await api.GET("/api/health", {});
  expect(response.status).toBe(200);
  expect(data?.status).toBe("ok");
  expect(data?.name).toBe("sure-api");
});

test("openapi document is served", async ({ server }) => {
  const res = await fetch(`${server.baseURL}/api/openapi.json`);
  expect(res.ok).toBe(true);
  const doc = (await res.json()) as { openapi: string; paths: Record<string, unknown> };
  expect(doc.openapi).toMatch(/^3\./);
  expect(doc.paths["/api/health"]).toBeTruthy();
});
