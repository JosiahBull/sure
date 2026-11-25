import { test, expect } from "../fixtures";
import { createCategory } from "../helpers";

test("mappings survive renames, disable on deletion, and round-trip in snapshots", async ({ api }) => {
  const target = await createCategory(api, "Everyday meals");
  const initial = await api.GET("/api/category-mappings", {});
  const mapping = initial.data!.find(m => m.source === "akahu" && m.source_name === "Cafes and restaurants")!;
  expect(mapping).toBeTruthy();
  const body = { source: mapping.source, source_group: mapping.source_group, source_name: mapping.source_name, category_id: target.id };
  expect((await api.PUT("/api/category-mappings/{id}", { params: { path: { id: mapping.id } }, body })).response.status).toBe(200);
  await api.PUT("/api/categories/{id}", { params: { path: { id: target.id } }, body: { name: "Eating out", kind: "expense" } });
  const renamed = (await api.GET("/api/category-mappings", {})).data!.find(m => m.id === mapping.id)!;
  expect(renamed.category_id).toBe(target.id);

  const snapshot = (await api.GET("/api/config/export", {})).data;
  expect((await api.PUT("/api/category-mappings/{id}", { params: { path: { id: mapping.id } }, body: { ...body, category_id: null } })).response.status).toBe(200);
  expect((await api.POST("/api/config/import", { body: snapshot as never })).response.status).toBe(200);
  expect((await api.GET("/api/category-mappings", {})).data!.find(m => m.id === mapping.id)!.category_id).toBe(target.id);
  await api.DELETE("/api/categories/{id}", { params: { path: { id: target.id } } });
  expect((await api.GET("/api/category-mappings", {})).data!.find(m => m.id === mapping.id)!.category_id).toBeNull();
});

test("source keys are normalised, unique per source, and validated", async ({ api }) => {
  const target = await createCategory(api, "Everyday groceries");
  const body = { source: " SAMPLE_FEED ", source_group: " Food ", source_name: " Prepared meals ", category_id: target.id };
  const created = await api.POST("/api/category-mappings", { body });
  expect(created.response.status).toBe(201);
  expect(created.data).toMatchObject({ source: "sample_feed", source_group: "Food", source_name: "Prepared meals", category_id: target.id });
  const duplicate = await api.POST("/api/category-mappings", { body: { ...body, source: "sample_feed", source_group: "food", source_name: "prepared MEALS" } });
  expect(duplicate.response.status).toBe(422);
  expect((await api.POST("/api/category-mappings", { body: { ...body, source: "another-feed" } })).response.status).toBe(201);
  expect((await api.POST("/api/category-mappings", { body: { ...body, source: "", source_name: "Invalid" } })).response.status).toBe(422);
  expect((await api.POST("/api/category-mappings", { body: { ...body, source_name: "Missing destination", category_id: Number.MAX_SAFE_INTEGER } })).response.status).toBe(422);
});
