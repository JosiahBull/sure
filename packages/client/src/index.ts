import createClient, { type Client } from "openapi-fetch";
import type { paths, components } from "./schema";

/** A fully-typed client for the Sure API, generated from the backend's OpenAPI spec. */
export type SureClient = Client<paths>;

/** Create a typed API client. `baseUrl` defaults to same-origin `/`. */
export function createSureClient(baseUrl = "/"): SureClient {
  return createClient<paths>({ baseUrl });
}

// Handy shorthands for the generated component schemas, e.g. `Schemas["Account"]`.
export type Schemas = components["schemas"];
export type { paths, components };
